export type RfbStatus =
  | { state: "idle" }
  | { state: "connecting" }
  | { state: "connected"; width: number; height: number; name: string }
  | { state: "error"; error: string }
  | { state: "closed" };

type PixelFormat = {
  bitsPerPixel: number;
  depth: number;
  bigEndian: number;
  trueColor: number;
  redMax: number;
  greenMax: number;
  blueMax: number;
  redShift: number;
  greenShift: number;
  blueShift: number;
};

type ServerInit = {
  width: number;
  height: number;
  name: string;
  pixelFormat: PixelFormat;
};

const RFB_VERSION_38 = "RFB 003.008\n";
function debugLog(
  hypothesisId: string,
  location: string,
  message: string,
  data: Record<string, unknown>,
) {
  void hypothesisId;
  void location;
  void message;
  void data;
}

// VNC keysyms: minimal set
const KEYSYM = {
  BackSpace: 0xff08,
  Tab: 0xff09,
  Enter: 0xff0d,
  Escape: 0xff1b,
  Insert: 0xff63,
  Delete: 0xffff,
  Home: 0xff50,
  End: 0xff57,
  PageUp: 0xff55,
  PageDown: 0xff56,
  ArrowLeft: 0xff51,
  ArrowUp: 0xff52,
  ArrowRight: 0xff53,
  ArrowDown: 0xff54,
  Shift: 0xffe1,
  Control: 0xffe3,
  Alt: 0xffe9,
  Meta: 0xffe7,
} as const;

class ByteQueue {
  private buf: Uint8Array<ArrayBufferLike> = new Uint8Array(0);
  private waiters: Array<() => void> = [];
  private isClosed = false;

  available() {
    return this.buf.length;
  }

  push(chunk: Uint8Array<ArrayBufferLike>) {
    if (this.isClosed) return;
    if (chunk.length === 0) return;
    if (this.buf.length === 0) {
      this.buf = chunk;
    } else {
      const merged: Uint8Array<ArrayBufferLike> = new Uint8Array(
        this.buf.length + chunk.length,
      );
      merged.set(this.buf, 0);
      merged.set(chunk, this.buf.length);
      this.buf = merged;
    }
    const w = this.waiters;
    this.waiters = [];
    for (const f of w) f();
  }

  close() {
    this.isClosed = true;
    const w = this.waiters;
    this.waiters = [];
    for (const f of w) f();
  }

  async readExactly(n: number): Promise<Uint8Array> {
    while (!this.isClosed && this.buf.length < n) {
      await new Promise<void>((resolve) => this.waiters.push(resolve));
    }
    if (this.buf.length < n) throw new Error("connection closed");
    const out = this.buf.slice(0, n);
    this.buf = this.buf.slice(n);
    return out;
  }
}

export class RfbClient {
  private dc: RTCDataChannel;
  private q = new ByteQueue();
  private ctx: CanvasRenderingContext2D;
  private canvas: HTMLCanvasElement;

  private fbWidth = 0;
  private fbHeight = 0;
  private pf: PixelFormat | null = null;
  private imageData: ImageData | null = null;
  private statusCb: (s: RfbStatus) => void;
  private plX = 0;
  private plY = 0;
  private lastAbsX = 0;
  private lastAbsY = 0;
  private loggedFirstChunk = false;
  private loggedChunkCount = 0;
  private loggedMsgType = 0;
  private loggedFbStart = false;
  private loggedFbHeader = false;
  private loggedFbRect = false;
  private loggedFbRaw = false;
  private loggedFbReq = false;
  private loggedUpdate = false;

  constructor(opts: {
    dc: RTCDataChannel;
    canvas: HTMLCanvasElement;
    statusCb: (s: RfbStatus) => void;
  }) {
    this.dc = opts.dc;
    this.canvas = opts.canvas;
    const ctx = this.canvas.getContext("2d", { alpha: false });
    if (!ctx) throw new Error("canvas 2d ctx unavailable");
    this.ctx = ctx;
    this.statusCb = opts.statusCb;

    this.dc.binaryType = "arraybuffer";
    this.dc.addEventListener("message", (ev) => {
      if (typeof ev.data === "string") return;
      if (ev.data instanceof ArrayBuffer) {
        if (!this.loggedFirstChunk) {
          this.loggedFirstChunk = true;
          debugLog("H", "src/lib/rfb.ts:dc.onmessage", "dc_message", {
            bytes: ev.data.byteLength,
            type: "arraybuffer",
          });
        }
        const chunk = new Uint8Array(ev.data);
        if (this.loggedChunkCount < 5) {
          // #region agent log
          debugLog("H", "src/lib/rfb.ts:dc.onmessage", "dc_message_sample", {
            bytes: chunk.byteLength,
            type: "arraybuffer",
            first: chunk[0] ?? 0,
            idx: this.loggedChunkCount + 1,
          });
          // #endregion
          this.loggedChunkCount += 1;
        }
        this.q.push(chunk);
        return;
      }
      if (ev.data instanceof Blob) {
        if (!this.loggedFirstChunk) {
          this.loggedFirstChunk = true;
          debugLog("H", "src/lib/rfb.ts:dc.onmessage", "dc_message", {
            bytes: ev.data.size,
            type: "blob",
          });
        }
        if (this.loggedChunkCount < 5) {
          // #region agent log
          debugLog("H", "src/lib/rfb.ts:dc.onmessage", "dc_message_sample", {
            bytes: ev.data.size,
            type: "blob",
            idx: this.loggedChunkCount + 1,
          });
          // #endregion
          this.loggedChunkCount += 1;
        }
        void ev.data
          .arrayBuffer()
          .then((ab) => this.q.push(new Uint8Array(ab)));
      }
    });
    this.dc.addEventListener("close", () => {
      this.q.close();
      this.statusCb({ state: "closed" });
    });
  }

  attachInput() {
    // keyboard
    window.addEventListener("keydown", this.onKeyDown, { passive: false });
    window.addEventListener("keyup", this.onKeyUp, { passive: false });

    // mouse
    this.canvas.addEventListener("mousemove", this.onMouseMove, {
      passive: false,
    });
    this.canvas.addEventListener("mousedown", this.onMouseDown, {
      passive: false,
    });
    this.canvas.addEventListener("mouseup", this.onMouseUp, { passive: false });
    this.canvas.addEventListener("click", this.onCanvasClick, {
      passive: false,
    });
    document.addEventListener("pointerlockchange", this.onPointerLockChange, {
      passive: true,
    });
    this.canvas.addEventListener("contextmenu", (e) => e.preventDefault());
  }

  detachInput() {
    window.removeEventListener("keydown", this.onKeyDown);
    window.removeEventListener("keyup", this.onKeyUp);
    this.canvas.removeEventListener("mousemove", this.onMouseMove);
    this.canvas.removeEventListener("mousedown", this.onMouseDown);
    this.canvas.removeEventListener("mouseup", this.onMouseUp);
    this.canvas.removeEventListener("click", this.onCanvasClick);
    document.removeEventListener("pointerlockchange", this.onPointerLockChange);
  }

  async start() {
    this.statusCb({ state: "connecting" });
    debugLog("E", "src/lib/rfb.ts:start", "handshake_start", {});
    await this.handshake();
    this.attachInput();
    this.statusCb({
      state: "connected",
      width: this.fbWidth,
      height: this.fbHeight,
      name: "",
    });
    void this.readLoop();
  }

  private async handshake() {
    await this.negotiateProtocol();
    await this.negotiateSecurity();
    const init = await this.readServerInit();
    this.initFramebuffer(init);
    this.sendPreferredPixelFormat();
    this.sendSetEncodings([0]); // RAW only
    this.sendFramebufferUpdateRequest(false, 0, 0, init.width, init.height);
    this.statusCb({
      state: "connected",
      width: init.width,
      height: init.height,
      name: init.name,
    });
  }

  private async readLoop() {
    try {
      while (true) {
        const msgType = await this.readU8();
        if (this.loggedMsgType < 5) {
          // #region agent log
          debugLog("I", "src/lib/rfb.ts:readLoop", "server_msg", {
            msgType,
            idx: this.loggedMsgType + 1,
          });
          // #endregion
          this.loggedMsgType += 1;
        }
        switch (msgType) {
          case 0:
            await this.handleFramebufferUpdate();
            this.sendFramebufferUpdateRequest(
              true,
              0,
              0,
              this.fbWidth,
              this.fbHeight,
            );
            break;
          case 2:
            break; // Bell
          case 3:
            await this.readServerCutText();
            break;
          default:
            throw new Error(`Unhandled server message: ${msgType}`);
        }
      }
    } catch (e) {
      // #region agent log
      debugLog("I", "src/lib/rfb.ts:readLoop", "read_loop_err", {
        error: e instanceof Error ? e.message : String(e),
      });
      // #endregion
      this.statusCb({
        state: "error",
        error: e instanceof Error ? e.message : String(e),
      });
      this.q.close();
    }
  }

  private async handleFramebufferUpdate() {
    if (!this.loggedFbStart) {
      // #region agent log
      debugLog(
        "J",
        "src/lib/rfb.ts:handleFramebufferUpdate",
        "fb_update_start",
        {
          avail: this.q.available(),
        },
      );
      // #endregion
      this.loggedFbStart = true;
    }
    // padding
    await this.readU8();
    const rects = await this.readU16();
    if (!this.loggedFbHeader) {
      // #region agent log
      debugLog(
        "J",
        "src/lib/rfb.ts:handleFramebufferUpdate",
        "fb_update_header",
        {
          rects,
          avail: this.q.available(),
        },
      );
      // #endregion
      this.loggedFbHeader = true;
    }
    if (!this.imageData) return;

    let firstRect: {
      x: number;
      y: number;
      w: number;
      h: number;
      enc: number;
    } | null = null;
    let dirtyX0 = 0;
    let dirtyY0 = 0;
    let dirtyX1 = 0;
    let dirtyY1 = 0;
    for (let i = 0; i < rects; i++) {
      const x = await this.readU16();
      const y = await this.readU16();
      const w = await this.readU16();
      const h = await this.readU16();
      const enc = await this.readI32();
      if (firstRect === null) {
        firstRect = { x, y, w, h, enc };
        // init dirty bounds
        dirtyX0 = x;
        dirtyY0 = y;
        dirtyX1 = x + w;
        dirtyY1 = y + h;
        if (!this.loggedFbRect) {
          // #region agent log
          debugLog(
            "J",
            "src/lib/rfb.ts:handleFramebufferUpdate",
            "fb_update_rect0",
            {
              x,
              y,
              w,
              h,
              enc,
              need: w * h * 4,
              avail: this.q.available(),
            },
          );
          // #endregion
          this.loggedFbRect = true;
        }
      } else {
        dirtyX0 = Math.min(dirtyX0, x);
        dirtyY0 = Math.min(dirtyY0, y);
        dirtyX1 = Math.max(dirtyX1, x + w);
        dirtyY1 = Math.max(dirtyY1, y + h);
      }

      if (enc !== 0) {
        // only RAW supported
        throw new Error(`Unsupported encoding: ${enc}`);
      }

      const bytesPerPixel = 4;
      const raw = await this.q.readExactly(w * h * bytesPerPixel);
      if (!this.loggedFbRaw) {
        let sample = 0;
        let nonZero = 0;
        const sampleMax = Math.min(raw.length, 256);
        for (let i = 0; i < sampleMax; i++) {
          sample += 1;
          if (raw[i] !== 0) nonZero += 1;
        }
        // #region agent log
        debugLog(
          "J",
          "src/lib/rfb.ts:handleFramebufferUpdate",
          "fb_update_raw_ok",
          {
            bytes: raw.length,
            avail: this.q.available(),
            sample,
            nonZero,
          },
        );
        // #endregion
        this.loggedFbRaw = true;
      }
      this.blitRaw32leRGBX(x, y, w, h, raw);
    }
    if (firstRect) {
      const dx = Math.max(0, Math.min(this.fbWidth, dirtyX0));
      const dy = Math.max(0, Math.min(this.fbHeight, dirtyY0));
      const dw = Math.max(0, Math.min(this.fbWidth, dirtyX1)) - dx;
      const dh = Math.max(0, Math.min(this.fbHeight, dirtyY1)) - dy;
      if (dw > 0 && dh > 0) {
        this.ctx.putImageData(this.imageData, 0, 0, dx, dy, dw, dh);
        // #region agent log
        debugLog("K", "src/lib/rfb.ts:handleFramebufferUpdate", "canvas_put", {
          rects,
          size: `${this.fbWidth}x${this.fbHeight}`,
          dirty: `${dx},${dy},${dw},${dh}`,
        });
        // #endregion
      } else {
        this.ctx.putImageData(this.imageData, 0, 0);
        // #region agent log
        debugLog("K", "src/lib/rfb.ts:handleFramebufferUpdate", "canvas_put", {
          rects,
          size: `${this.fbWidth}x${this.fbHeight}`,
          dirty: "full",
        });
        // #endregion
      }
    } else {
      this.ctx.putImageData(this.imageData, 0, 0);
      // #region agent log
      debugLog("K", "src/lib/rfb.ts:handleFramebufferUpdate", "canvas_put", {
        rects,
        size: `${this.fbWidth}x${this.fbHeight}`,
        dirty: "full",
      });
      // #endregion
    }
    if (!this.loggedUpdate) {
      // #region agent log
      debugLog("U", "src/lib/rfb.ts:handleFramebufferUpdate", "fb_update", {
        rects,
        firstRect,
      });
      // #endregion
      this.loggedUpdate = true;
    }
  }

  private blitRaw32leRGBX(
    x: number,
    y: number,
    w: number,
    h: number,
    raw: Uint8Array,
  ) {
    if (!this.imageData) return;
    const dst = this.imageData.data;
    const fbW = this.fbWidth;

    let srcOff = 0;
    for (let row = 0; row < h; row++) {
      let dstOff = ((y + row) * fbW + x) * 4;
      for (let col = 0; col < w; col++) {
        const b = raw[srcOff + 0]!;
        const g = raw[srcOff + 1]!;
        const r = raw[srcOff + 2]!;
        // raw[srcOff + 3] is unused (X)
        dst[dstOff + 0] = r;
        dst[dstOff + 1] = g;
        dst[dstOff + 2] = b;
        dst[dstOff + 3] = 255;
        srcOff += 4;
        dstOff += 4;
      }
    }
  }

  private async readServerCutText() {
    await this.q.readExactly(3); // padding
    const len = await this.readU32();
    await this.q.readExactly(len);
  }

  private sendSetPixelFormat(pf: PixelFormat) {
    const b = new Uint8Array(1 + 3 + 16);
    b[0] = 0;
    // 3 bytes padding already 0
    this.writePixelFormat(b, 4, pf);
    this.send(b);
  }

  private sendSetEncodings(encs: number[]) {
    const b = new Uint8Array(1 + 1 + 2 + encs.length * 4);
    b[0] = 2;
    // padding at [1]
    this.writeU16(b, 2, encs.length);
    let off = 4;
    for (const e of encs) {
      this.writeI32(b, off, e);
      off += 4;
    }
    this.send(b);
  }

  private sendFramebufferUpdateRequest(
    incremental: boolean,
    x: number,
    y: number,
    w: number,
    h: number,
  ) {
    if (!this.loggedFbReq) {
      // #region agent log
      debugLog(
        "U",
        "src/lib/rfb.ts:sendFramebufferUpdateRequest",
        "fb_request",
        {
          incremental,
          x,
          y,
          w,
          h,
        },
      );
      // #endregion
      this.loggedFbReq = true;
    }
    const b = new Uint8Array(1 + 1 + 2 + 2 + 2 + 2);
    b[0] = 3;
    b[1] = incremental ? 1 : 0;
    this.writeU16(b, 2, x);
    this.writeU16(b, 4, y);
    this.writeU16(b, 6, w);
    this.writeU16(b, 8, h);
    this.send(b);
  }

  private onKeyDown = (ev: KeyboardEvent) => {
    if (ev.repeat) return;
    const keysym = this.keysymFromEvent(ev);
    if (keysym == null) return;
    ev.preventDefault();
    this.sendKeyEvent(true, keysym);
  };

  private onKeyUp = (ev: KeyboardEvent) => {
    const keysym = this.keysymFromEvent(ev);
    if (keysym == null) return;
    ev.preventDefault();
    this.sendKeyEvent(false, keysym);
  };

  private keysymFromEvent(ev: KeyboardEvent): number | null {
    const k = ev.key;
    if (k.length === 1) return k.codePointAt(0) ?? null;
    if (k in KEYSYM) return (KEYSYM as Record<string, number>)[k] ?? null;
    // common aliases
    if (k === "Esc") return KEYSYM.Escape;
    return null;
  }

  private sendKeyEvent(down: boolean, keysym: number) {
    const b = new Uint8Array(1 + 1 + 2 + 4);
    b[0] = 4;
    b[1] = down ? 1 : 0;
    // padding [2..3]
    this.writeU32(b, 4, keysym >>> 0);
    this.send(b);
  }

  private mouseButtonsMask(ev: MouseEvent): number {
    // buttons: bit 0=left,1=right,2=middle in DOM; VNC mask: 1=left,2=middle,4=right
    let mask = 0;
    if (ev.buttons & 1) mask |= 1;
    if (ev.buttons & 4) mask |= 2;
    if (ev.buttons & 2) mask |= 4;
    return mask;
  }

  private pointerPos(ev: MouseEvent): { x: number; y: number } {
    const r = this.contentRect();
    const sx = this.fbWidth / r.width;
    const sy = this.fbHeight / r.height;
    const x = Math.max(
      0,
      Math.min(this.fbWidth - 1, Math.floor((ev.clientX - r.left) * sx)),
    );
    const y = Math.max(
      0,
      Math.min(this.fbHeight - 1, Math.floor((ev.clientY - r.top) * sy)),
    );
    return { x, y };
  }

  private contentRect(): {
    left: number;
    top: number;
    width: number;
    height: number;
  } {
    const r = this.canvas.getBoundingClientRect();
    if (this.fbWidth <= 0 || this.fbHeight <= 0) return r;

    const fbAspect = this.fbWidth / this.fbHeight;
    const boxAspect = r.width / r.height;

    // canvas is styled with object-contain; compute the displayed content rect (letterboxing)
    let w = r.width;
    let h = r.height;
    if (boxAspect > fbAspect) {
      // box too wide
      h = r.height;
      w = h * fbAspect;
    } else {
      // box too tall
      w = r.width;
      h = w / fbAspect;
    }
    const left = r.left + (r.width - w) / 2;
    const top = r.top + (r.height - h) / 2;
    return { left, top, width: w, height: h };
  }

  private onCanvasClick = (ev: MouseEvent) => {
    // Pointer Lock requires a user gesture; click is simplest cross-browser way.
    if (document.pointerLockElement === this.canvas) return;
    // Don't steal focus if not interacting.
    if (ev.button !== 0) return;
    ev.preventDefault();
    this.canvas.requestPointerLock();
  };

  private onPointerLockChange = () => {
    if (document.pointerLockElement === this.canvas) {
      // Initialize virtual pointer at last known absolute position.
      this.plX = this.lastAbsX || Math.floor(this.fbWidth / 2);
      this.plY = this.lastAbsY || Math.floor(this.fbHeight / 2);
    }
  };

  private onMouseMove = (ev: MouseEvent) => {
    ev.preventDefault();
    if (document.pointerLockElement === this.canvas) {
      const r = this.contentRect();
      const sx = this.fbWidth / r.width;
      const sy = this.fbHeight / r.height;
      this.plX = Math.max(
        0,
        Math.min(this.fbWidth - 1, this.plX + ev.movementX * sx),
      );
      this.plY = Math.max(
        0,
        Math.min(this.fbHeight - 1, this.plY + ev.movementY * sy),
      );
      this.sendPointerLocked(ev);
      return;
    }
    const p = this.pointerPos(ev);
    this.sendPointerForEvent(ev, p.x, p.y);
  };

  private onMouseDown = (ev: MouseEvent) => {
    ev.preventDefault();
    if (document.pointerLockElement === this.canvas) {
      this.sendPointerLocked(ev);
      return;
    }
    const p = this.pointerPos(ev);
    this.sendPointerForEvent(ev, p.x, p.y);
  };

  private onMouseUp = (ev: MouseEvent) => {
    ev.preventDefault();
    if (document.pointerLockElement === this.canvas) {
      this.sendPointerLocked(ev);
      return;
    }
    const p = this.pointerPos(ev);
    this.sendPointerForEvent(ev, p.x, p.y);
  };

  private sendPointerLocked(ev: MouseEvent) {
    this.sendPointerEvent(
      this.mouseButtonsMask(ev),
      Math.floor(this.plX),
      Math.floor(this.plY),
    );
  }

  private sendPointerForEvent(ev: MouseEvent, x: number, y: number) {
    this.lastAbsX = x;
    this.lastAbsY = y;
    this.sendPointerEvent(this.mouseButtonsMask(ev), x, y);
  }

  private sendPointerEvent(mask: number, x: number, y: number) {
    const b = new Uint8Array(1 + 1 + 2 + 2);
    b[0] = 5;
    b[1] = mask & 0xff;
    this.writeU16(b, 2, x);
    this.writeU16(b, 4, y);
    this.send(b);
  }

  private sendAscii(s: string) {
    this.send(new TextEncoder().encode(s));
  }

  private async negotiateProtocol() {
    debugLog("E", "src/lib/rfb.ts:negotiateProtocol", "protocol_wait", {});
    const ver = new TextDecoder().decode(await this.q.readExactly(12));
    if (!ver.startsWith("RFB ")) throw new Error(`bad RFB version: ${ver}`);
    this.sendAscii(RFB_VERSION_38);
    debugLog("E", "src/lib/rfb.ts:negotiateProtocol", "protocol_ok", { ver });
  }

  private async negotiateSecurity() {
    const nSec = await this.readU8();
    if (nSec === 0) {
      const len = await this.readU32();
      const reason = new TextDecoder().decode(await this.q.readExactly(len));
      throw new Error(`RFB security failed: ${reason}`);
    }

    const secTypes = await this.q.readExactly(nSec);
    const hasNone = secTypes.includes(1);
    if (!hasNone)
      throw new Error("RFB: server does not support 'None' security");
    this.sendU8(1); // None

    const secResult = await this.readU32();
    if (secResult !== 0) throw new Error(`RFB security result: ${secResult}`);

    this.sendU8(1); // shared-flag
    debugLog("F", "src/lib/rfb.ts:negotiateSecurity", "security_ok", {
      nSec,
      secResult,
    });
  }

  private async readServerInit(): Promise<ServerInit> {
    const width = await this.readU16();
    const height = await this.readU16();
    const pixelFormat = await this.readPixelFormat();
    const nameLen = await this.readU32();
    const name = new TextDecoder().decode(await this.q.readExactly(nameLen));
    debugLog("G", "src/lib/rfb.ts:readServerInit", "server_init", {
      width,
      height,
      nameLen,
    });
    return { width, height, name, pixelFormat };
  }

  private initFramebuffer(init: ServerInit) {
    this.fbWidth = init.width;
    this.fbHeight = init.height;
    this.pf = init.pixelFormat;
    this.canvas.width = init.width;
    this.canvas.height = init.height;
    this.imageData = this.ctx.createImageData(init.width, init.height);
  }

  private sendPreferredPixelFormat() {
    this.sendSetPixelFormat({
      bitsPerPixel: 32,
      depth: 24,
      bigEndian: 0,
      trueColor: 1,
      redMax: 255,
      greenMax: 255,
      blueMax: 255,
      redShift: 16,
      greenShift: 8,
      blueShift: 0,
    });
  }

  private sendU8(v: number) {
    this.send(Uint8Array.of(v & 0xff));
  }

  private send(buf: Uint8Array) {
    if (this.dc.readyState !== "open") return;
    // Work around TS typed-array generic mismatch (ArrayBuffer vs ArrayBufferLike).
    // We always want to send an ArrayBuffer-backed view.
    const copy = new Uint8Array(buf.byteLength);
    copy.set(buf);
    this.dc.send(copy);
  }

  private async readU8() {
    return (await this.q.readExactly(1))[0]!;
  }

  private async readU16() {
    const b = await this.q.readExactly(2);
    return new DataView(b.buffer, b.byteOffset, b.byteLength).getUint16(
      0,
      false,
    );
  }

  private async readU32() {
    const b = await this.q.readExactly(4);
    return new DataView(b.buffer, b.byteOffset, b.byteLength).getUint32(
      0,
      false,
    );
  }

  private async readI32() {
    const b = await this.q.readExactly(4);
    return new DataView(b.buffer, b.byteOffset, b.byteLength).getInt32(
      0,
      false,
    );
  }

  private async readPixelFormat(): Promise<PixelFormat> {
    const b = await this.q.readExactly(16);
    const dv = new DataView(b.buffer, b.byteOffset, b.byteLength);
    const bitsPerPixel = dv.getUint8(0);
    const depth = dv.getUint8(1);
    const bigEndian = dv.getUint8(2);
    const trueColor = dv.getUint8(3);
    const redMax = dv.getUint16(4, false);
    const greenMax = dv.getUint16(6, false);
    const blueMax = dv.getUint16(8, false);
    const redShift = dv.getUint8(10);
    const greenShift = dv.getUint8(11);
    const blueShift = dv.getUint8(12);
    return {
      bitsPerPixel,
      depth,
      bigEndian,
      trueColor,
      redMax,
      greenMax,
      blueMax,
      redShift,
      greenShift,
      blueShift,
    };
  }

  private writePixelFormat(b: Uint8Array, off: number, pf: PixelFormat) {
    b[off + 0] = pf.bitsPerPixel & 0xff;
    b[off + 1] = pf.depth & 0xff;
    b[off + 2] = pf.bigEndian & 0xff;
    b[off + 3] = pf.trueColor & 0xff;
    this.writeU16(b, off + 4, pf.redMax);
    this.writeU16(b, off + 6, pf.greenMax);
    this.writeU16(b, off + 8, pf.blueMax);
    b[off + 10] = pf.redShift & 0xff;
    b[off + 11] = pf.greenShift & 0xff;
    b[off + 12] = pf.blueShift & 0xff;
    b[off + 13] = 0;
    b[off + 14] = 0;
    b[off + 15] = 0;
  }

  private writeU16(b: Uint8Array, off: number, v: number) {
    b[off] = (v >>> 8) & 0xff;
    b[off + 1] = v & 0xff;
  }

  private writeU32(b: Uint8Array, off: number, v: number) {
    b[off] = (v >>> 24) & 0xff;
    b[off + 1] = (v >>> 16) & 0xff;
    b[off + 2] = (v >>> 8) & 0xff;
    b[off + 3] = v & 0xff;
  }

  private writeI32(b: Uint8Array, off: number, v: number) {
    this.writeU32(b, off, v >>> 0);
  }
}
