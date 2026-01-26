<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { RfbClient, type RfbStatus } from "$lib/rfb";

  let canvasEl: HTMLCanvasElement | null = null;
  let status: RfbStatus = { state: "idle" };
  let statusText = "Idle";

  let pc: RTCPeerConnection | null = null;
  let dc: RTCDataChannel | null = null;
  let rfb: RfbClient | null = null;
  let sessionId: string | null = null;
  let pollTimer: number | null = null;
  let pendingLocal: RTCIceCandidateInit[] = [];
  function debugLog(hypothesisId: string, location: string, message: string, data: Record<string, unknown>) {
    void hypothesisId;
    void location;
    void message;
    void data;
  }

  const statusLabel = (s: RfbStatus) => {
    switch (s.state) {
      case "idle":
        return "Idle";
      case "connecting":
        return "Connecting…";
      case "connected":
        return "Connected";
      case "closed":
        return "Closed";
      case "error":
        return `Error: ${s.error}`;
    }
  };

  const setStatus = (s: RfbStatus) => {
    // #region agent log
    if (s.state !== status.state) {
      debugLog("S", "src/routes/+page.svelte:setStatus", "status_update", {
        state: s.state,
        prev: status.state,
        width: (s as { width?: number }).width ?? null,
        height: (s as { height?: number }).height ?? null,
      });
    }
    // #endregion
    status = s;
    statusText = statusLabel(s);
  };

  async function postJson<T>(path: string, body: unknown, attempt = 0): Promise<T> {
    const res = await fetch(path, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
    });
    if ((res.status === 409 || res.status === 503) && attempt < 1) {
      await new Promise((resolve) => setTimeout(resolve, 50));
      return postJson<T>(path, body, attempt + 1);
    }
    if (!res.ok) {
      const text = await res.text().catch(() => "");
      throw new Error(`${res.status} ${res.statusText}${text ? `: ${text}` : ""}`);
    }
    return (await res.json()) as T;
  }

  async function getJson<T>(path: string): Promise<T> {
    const sep = path.includes("?") ? "&" : "?";
    const url = `${path}${sep}t=${Date.now()}`;
    const res = await fetch(url, {
      cache: "no-store",
      headers: { accept: "application/json" },
    });
    if (!res.ok) {
      const text = await res.text().catch(() => "");
      throw new Error(`${res.status} ${res.statusText}${text ? `: ${text}` : ""}`);
    }

    const ct = res.headers.get("content-type") ?? "";
    if (!ct.includes("application/json")) {
      const text = await res.text().catch(() => "");
      // In dev, if the Vite proxy isn't active, /api will fall back to index.html.
      // Retry directly against the backend for a better UX.
      if (location.hostname === "localhost" && location.port === "5173") {
        const direct = await fetch(`http://localhost:8080${url}`, {
          cache: "no-store",
          headers: { accept: "application/json" },
        });
        if (direct.ok) return (await direct.json()) as T;
      }
      throw new Error(`Expected JSON from ${res.url}, got ${ct}: ${text.slice(0, 80)}`);
    }

    return (await res.json()) as T;
  }

  function toTrickle(c: RTCIceCandidateInit) {
    return {
      candidate: c.candidate ?? "",
      sdp_mid: c.sdpMid ?? null,
      sdp_mline_index: (c.sdpMLineIndex ?? null) as number | null,
      username_fragment: (c.usernameFragment ?? null) as string | null,
    };
  }

  async function syncCandidates(candidate: RTCIceCandidateInit | null) {
    if (!pc || !sessionId) return;
    const res = await postJson<{ candidates: Array<{ candidate: string; sdp_mid: string | null; sdp_mline_index: number | null; username_fragment: string | null }> }>(
      "/api/webrtc/candidate",
      { session_id: sessionId, candidate: candidate ? toTrickle(candidate) : null },
    );
    for (const c of res.candidates) {
      await pc.addIceCandidate({
        candidate: c.candidate,
        sdpMid: c.sdp_mid ?? undefined,
        sdpMLineIndex: c.sdp_mline_index ?? undefined,
        usernameFragment: c.username_fragment ?? undefined,
      });
    }
  }

  function initPeerConnection(iceServers: Array<{ urls: string[]; username?: string | null; credential?: string | null }>) {
    pc = new RTCPeerConnection({
      iceServers: iceServers.map((s) => ({
        urls: s.urls,
        username: s.username ?? undefined,
        credential: s.credential ?? undefined,
      })),
    });
    dc = pc.createDataChannel("vnc", { ordered: true });

    pc.onicecandidate = (ev) => {
      if (!ev.candidate) return;
      const init = ev.candidate.toJSON();
      if (!sessionId) {
        pendingLocal.push(init);
        return;
      }
      void syncCandidates(init);
    };

    dc.onopen = () => {
      debugLog("D", "src/routes/+page.svelte:dc.onopen", "dc_open", { label: dc?.label ?? "unknown" });
      if (!dc || !canvasEl) return;
      rfb = new RfbClient({ dc, canvas: canvasEl, statusCb: setStatus });
      void rfb.start().catch((err) => {
        setStatus({ state: "error", error: err instanceof Error ? err.message : String(err) });
      });
    };

    dc.onclose = () => setStatus({ state: "closed" });
  }

  async function startSession() {
    if (!pc) throw new Error("peer connection missing");
    const offer = await pc.createOffer();
    await pc.setLocalDescription(offer);

    const answer = await postJson<{ session_id: string; sdp: string; type: string }>(
      "/api/webrtc/offer",
      { type: offer.type, sdp: offer.sdp },
    );
    sessionId = answer.session_id;
    debugLog("A", "src/routes/+page.svelte:startSession", "offer_answered", { sessionId });

    await pc.setRemoteDescription({
      type: answer.type as RTCSessionDescriptionInit["type"],
      sdp: answer.sdp,
    });

    for (const c of pendingLocal) {
      await syncCandidates(c);
    }
    pendingLocal = [];

    pollTimer = window.setInterval(() => {
      void syncCandidates(null);
    }, 250);
  }

  async function connect() {
    if (!canvasEl) throw new Error("canvas missing");
    setStatus({ state: "connecting" });
    debugLog("A", "src/routes/+page.svelte:connect", "connect_start", {});

    const webrtcCfg = await getJson<{
      ice_servers: Array<{ urls: string[]; username?: string | null; credential?: string | null }>;
    }>("/api/webrtc/config");
    debugLog("C", "src/routes/+page.svelte:connect", "config_loaded", {
      ice_servers: webrtcCfg.ice_servers.length,
    });

    initPeerConnection(webrtcCfg.ice_servers);
    await startSession();
  }

  onMount(() => {
    void connect().catch((e) => {
      setStatus({ state: "error", error: e instanceof Error ? e.message : String(e) });
    });
  });

  onDestroy(() => {
    if (pollTimer) window.clearInterval(pollTimer);
    rfb?.detachInput();
    try {
      dc?.close();
    } catch {
      // ignore
    }
    try {
      pc?.close();
    } catch {
      // ignore
    }
  });
</script>

<svelte:head>
  <title>Web OS</title>
  <link rel="icon" type="image/x-icon" href="/favicon.ico" />
  <link rel="apple-touch-icon" href="/apple-touch-icon.png" />
  <link rel="preconnect" href="https://fonts.googleapis.com" />
  <link rel="preconnect" href="https://fonts.gstatic.com" crossorigin="anonymous" />
  <link href="https://fonts.googleapis.com/css2?family=VT323&display=swap" rel="stylesheet" />
</svelte:head>

<div class="min-h-screen p-8 bg-black text-crt-green font-retro flex flex-col">
  <header class="flex items-center justify-end gap-4 pb-8">
    <a
      href="https://github.com/andrewhinh/web-os"
      class="inline-flex size-10 items-center justify-center rounded-full border border-crt-green/40 opacity-80 transition hover:border-crt-green hover:opacity-100 hover:bg-crt-green/10"
      target="_blank"
      rel="noreferrer"
      aria-label="GitHub repository"
    >
      <span class="sr-only">GitHub</span>
      <svg class="size-10" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
        <path d="M12 .5C5.65.5.5 5.65.5 12a11.5 11.5 0 0 0 7.86 10.92c.58.11.8-.25.8-.56 0-.27-.01-.99-.02-1.94-3.2.7-3.88-1.52-3.88-1.52-.53-1.34-1.28-1.7-1.28-1.7-1.05-.72.08-.71.08-.71 1.16.08 1.77 1.19 1.77 1.19 1.05 1.77 2.74 1.26 3.41.96.11-.76.41-1.25.74-1.54-2.55-.29-5.23-1.28-5.23-5.68 0-1.25.45-2.29 1.2-3.09-.12-.29-.52-1.44.12-3 0 0 .98-.31 3.2 1.2a11.2 11.2 0 0 1 5.82 0c2.22-1.51 3.2-1.2 3.2-1.2.64 1.56.24 2.71.12 3 .75.8 1.2 1.84 1.2 3.09 0 4.41-2.68 5.38-5.24 5.67.42.36.8 1.05.8 2.13 0 1.54-.01 2.78-.01 3.16 0 .31.21.68.81.56A11.5 11.5 0 0 0 23.5 12C23.5 5.65 18.35.5 12 .5Z" />
      </svg>
    </a>
    <a
      href="https://ajhinh.com"
      class="text-xl font-semibold underline underline-offset-6 opacity-70 transition hover:opacity-100"
      target="_blank"
      rel="noreferrer"
    >
      Andrew Hinh
    </a>
  </header>

  <main class="flex flex-1 flex-col items-center justify-center">
    <div class="w-full max-w-5xl">
      <section class="relative w-full aspect-4/3 [&_canvas]:size-full! [&_canvas]:object-contain">
        <canvas bind:this={canvasEl} class="block bg-black cursor-none"></canvas>

        {#if status.state !== "connected"}
          <div class="absolute inset-0 flex items-center justify-center text-2xl bg-black/60">
            {statusText}
          </div>
        {/if}
      </section>
    </div>
  </main>
</div>
