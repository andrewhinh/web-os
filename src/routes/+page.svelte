<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { RfbClient, type RfbStatus } from "$lib/rfb";

  type MetricsSnapshot = {
    visitors: number;
    run_cmds: number;
  };

  let canvasEl: HTMLCanvasElement | null = null;
  let status: RfbStatus = { state: "idle" };
  let statusText = "Idle";

  let pc: RTCPeerConnection | null = null;
  let dc: RTCDataChannel | null = null;
  let rfb: RfbClient | null = null;
  let sessionId: string | null = null;
  let iceSource: EventSource | null = null;
  let pendingLocal: RTCIceCandidateInit[] = [];
  let metricsSource: EventSource | null = null;
  let reconnecting = false;
  let isDestroyed = false;

  let metrics: MetricsSnapshot | null = null;
  let metricsError: string | null = null;

  const formatOrdinal = (value: number) => {
    const mod100 = value % 100;
    if (mod100 >= 11 && mod100 <= 13) return `${value}th`;
    switch (value % 10) {
      case 1:
        return `${value}st`;
      case 2:
        return `${value}nd`;
      case 3:
        return `${value}rd`;
      default:
        return `${value}th`;
    }
  };

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

  async function trackVisit() {
    try {
      metrics = await postJson<MetricsSnapshot>("/api/metrics/visit", {});
      metricsError = null;
    } catch (err) {
      metricsError = err instanceof Error ? err.message : String(err);
    }
  }

  async function trackRunCmd() {
    try {
      metrics = await postJson<MetricsSnapshot>("/api/metrics/run-cmd", {});
      metricsError = null;
    } catch (err) {
      metricsError = err instanceof Error ? err.message : String(err);
    }
  }

  function toTrickle(c: RTCIceCandidateInit) {
    return {
      candidate: c.candidate ?? "",
      sdp_mid: c.sdpMid ?? null,
      sdp_mline_index: (c.sdpMLineIndex ?? null) as number | null,
      username_fragment: (c.usernameFragment ?? null) as string | null,
    };
  }

  async function syncCandidates(candidate: RTCIceCandidateInit) {
    if (!pc || !sessionId) return;
    await postJson<{ candidates: Array<{ candidate: string; sdp_mid: string | null; sdp_mline_index: number | null; username_fragment: string | null }> }>(
      "/api/webrtc/candidate",
      { session_id: sessionId, candidate: candidate ? toTrickle(candidate) : null },
    );
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
    pc.oniceconnectionstatechange = () => {
      if (!pc) return;
      if (pc.iceConnectionState === "failed" || pc.iceConnectionState === "disconnected") {
        void scheduleReconnect(`ice ${pc.iceConnectionState}`);
      }
    };
    pc.onconnectionstatechange = () => {
      if (!pc) return;
      if (pc.connectionState === "failed" || pc.connectionState === "disconnected") {
        void scheduleReconnect(`pc ${pc.connectionState}`);
      }
    };

    dc.onopen = () => {
      if (!dc || !canvasEl) return;
      rfb = new RfbClient({
        dc,
        canvas: canvasEl,
        statusCb: setStatus,
        onCommand: () => void trackRunCmd(),
      });
      void rfb.start().catch((err) => {
        setStatus({ state: "error", error: err instanceof Error ? err.message : String(err) });
      });
    };

    dc.onclose = () => setStatus({ state: "closed" });
    dc.onerror = () => {
      setStatus({ state: "error", error: "data channel error" });
      void scheduleReconnect("dc error");
    };
  }

  function cleanupConnection() {
    iceSource?.close();
    iceSource = null;
    pendingLocal = [];
    sessionId = null;
    rfb?.detachInput();
    rfb = null;
    try {
      dc?.close();
    } catch {
      // ignore
    }
    dc = null;
    try {
      pc?.close();
    } catch {
      // ignore
    }
    pc = null;
  }

  async function scheduleReconnect(reason: string) {
    void reason;
    if (reconnecting || isDestroyed) return;
    reconnecting = true;
    setStatus({ state: "connecting" });
    cleanupConnection();
    await new Promise((resolve) => setTimeout(resolve, 200));
    try {
      await connect();
    } catch (err) {
      setStatus({ state: "error", error: err instanceof Error ? err.message : String(err) });
    } finally {
      reconnecting = false;
    }
  }

  function startCandidateStream() {
    if (!sessionId) return;
    const qs = new URLSearchParams({ session_id: sessionId, t: String(Date.now()) });
    iceSource = new EventSource(`/api/webrtc/stream?${qs.toString()}`);
    iceSource.onmessage = (event) => {
      if (!pc) return;
      try {
        const c = JSON.parse(event.data) as {
          candidate: string;
          sdp_mid?: string | null;
          sdp_mline_index?: number | null;
          username_fragment?: string | null;
        };
        void pc.addIceCandidate({
          candidate: c.candidate,
          sdpMid: c.sdp_mid ?? undefined,
          sdpMLineIndex: c.sdp_mline_index ?? undefined,
          usernameFragment: c.username_fragment ?? undefined,
        });
      } catch {
        // ignore parse errors
      }
    };
    iceSource.onerror = () => {
      if (!pc) return;
      if (pc.iceConnectionState === "failed" || pc.connectionState === "failed") {
        void scheduleReconnect("sse error");
      }
    };
  }

  async function startSession() {
    if (!pc) throw new Error("peer connection missing");
    const offer = await pc.createOffer({
      offerToReceiveAudio: false,
      offerToReceiveVideo: false,
    });
    await pc.setLocalDescription(offer);

    const answer = await postJson<{ session_id: string; sdp: string; type: string }>(
      "/api/webrtc/offer",
      { type: offer.type, sdp: offer.sdp },
    );
    sessionId = answer.session_id;

    await pc.setRemoteDescription({
      type: answer.type as RTCSessionDescriptionInit["type"],
      sdp: answer.sdp,
    });

    startCandidateStream();

    for (const c of pendingLocal) {
      await syncCandidates(c);
    }
    pendingLocal = [];
  }

  async function connect() {
    if (!canvasEl) throw new Error("canvas missing");
    setStatus({ state: "connecting" });

    const webrtcCfg = await getJson<{
      ice_servers: Array<{ urls: string[]; username?: string | null; credential?: string | null }>;
    }>("/api/webrtc/config");

    initPeerConnection(webrtcCfg.ice_servers);
    await startSession();
  }

  onMount(() => {
    void connect().catch((e) => {
      setStatus({ state: "error", error: e instanceof Error ? e.message : String(e) });
    });
    void trackVisit();
    metricsSource = new EventSource("/api/metrics/stream");
    metricsSource.onopen = () => {
      metricsError = null;
    };
    metricsSource.onmessage = (event) => {
      try {
        metrics = JSON.parse(event.data) as MetricsSnapshot;
        metricsError = null;
      } catch (err) {
        metricsError = err instanceof Error ? err.message : String(err);
      }
    };
    metricsSource.onerror = () => {
      metricsError = "metrics stream error";
    };
  });

  onDestroy(() => {
    isDestroyed = true;
    metricsSource?.close();
    iceSource?.close();
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
  <header class="flex items-center justify-between gap-4 pb-8">
    <div class="flex flex-col gap-1 leading-none">
      <div class="text-xl">
        Hi visitor
        <span class="underline underline-offset-4">
          {metrics ? metrics.visitors : "—"}
        </span>!
      </div>
      <div class="text-xl">
        Run the
        <span class="underline underline-offset-4">
          {metrics ? formatOrdinal(metrics.run_cmds) : "—"}
        </span>
        command!
      </div>
      {#if metricsError}
        <span class="text-[10px] uppercase tracking-wide opacity-50">metrics offline</span>
      {/if}
    </div>
    <div class="flex items-center gap-4">
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
    </div>
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
