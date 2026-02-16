import { Unzlib } from "fflate";

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
const RFB_ENCODING_RAW = 0;
const RFB_ENCODING_ZLIB = 6;

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
  private chunks: Array<Uint8Array<ArrayBufferLike>> = [];
  private head = 0;
  private headOffset = 0;
  private buffered = 0;
  private waiters: Array<() => void> = [];
  private isClosed = false;

  push(chunk: Uint8Array<ArrayBufferLike>) {
    if (this.isClosed) return;
    if (chunk.length === 0) return;
    this.chunks.push(chunk);
    this.buffered += chunk.length;
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
    while (!this.isClosed && this.buffered < n) {
      await new Promise<void>((resolve) => this.waiters.push(resolve));
    }
    if (this.buffered < n) throw new Error("connection closed");

    const out = new Uint8Array(n);
    let written = 0;
    while (written < n) {
      const chunk = this.chunks[this.head];
      const avail = chunk.length - this.headOffset;
      const take = Math.min(avail, n - written);
      out.set(chunk.subarray(this.headOffset, this.headOffset + take), written);
      written += take;
      this.headOffset += take;
      this.buffered -= take;
      if (this.headOffset === chunk.length) {
        this.head += 1;
        this.headOffset = 0;
      }
    }

    if (this.head > 64 && this.head * 2 > this.chunks.length) {
      this.chunks = this.chunks.slice(this.head);
      this.head = 0;
    }
    return out;
  }
}

export class RfbClient {
  private dc: RTCDataChannel;
  private q = new ByteQueue();
  private ctx: CanvasRenderingContext2D;
  private canvas: HTMLCanvasElement;
  private keyboardInput: HTMLInputElement | null;
  private onCommand?: () => void;

  private fbWidth = 0;
  private fbHeight = 0;
  private pf: PixelFormat | null = null;
  private imageData: ImageData | null = null;
  private statusCb: (s: RfbStatus) => void;
  private plX = 0;
  private plY = 0;
  private lastAbsX = 0;
  private lastAbsY = 0;
  private touchId: number | null = null;
  private zlibStream: Unzlib | null = null;
  private zlibTarget: Uint8Array | null = null;
  private zlibTargetOff = 0;
  private pendingCommandChars = 0;
  private sendQueue: Array<Uint8Array<ArrayBuffer>> = [];
  private sendQueueHead = 0;
  private readonly bufferedLow = 256 * 1024;
  private readonly bufferedHigh = 1024 * 1024;
  private ctrlDown = false;
  private ctrlSynth = false;
  private framePresentPending = false;

  constructor(opts: {
    dc: RTCDataChannel;
    canvas: HTMLCanvasElement;
    keyboardInput?: HTMLInputElement | null;
    statusCb: (s: RfbStatus) => void;
    onCommand?: () => void;
  }) {
    this.dc = opts.dc;
    this.canvas = opts.canvas;
    this.keyboardInput = opts.keyboardInput ?? null;
    const ctx = this.canvas.getContext("2d", { alpha: false });
    if (!ctx) throw new Error("canvas 2d ctx unavailable");
    this.ctx = ctx;
    this.statusCb = opts.statusCb;
    this.onCommand = opts.onCommand;

    this.dc.binaryType = "arraybuffer";
    this.dc.bufferedAmountLowThreshold = this.bufferedLow;
    this.dc.addEventListener("message", (ev) => {
      if (typeof ev.data === "string") return;
      if (ev.data instanceof ArrayBuffer) {
        this.q.push(new Uint8Array(ev.data));
        return;
      }
      if (ev.data instanceof Blob) {
        void ev.data
          .arrayBuffer()
          .then((ab) => this.q.push(new Uint8Array(ab)));
      }
    });
    this.dc.addEventListener("bufferedamountlow", () => {
      this.flushSendQueue();
    });
    this.dc.addEventListener("close", () => {
      this.q.close();
      this.sendQueue = [];
      this.sendQueueHead = 0;
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
    this.canvas.addEventListener("wheel", this.onWheel, { passive: false });
    this.canvas.addEventListener("touchstart", this.onTouchStart, {
      passive: false,
    });
    this.canvas.addEventListener("touchmove", this.onTouchMove, {
      passive: false,
    });
    this.canvas.addEventListener("touchend", this.onTouchEnd, {
      passive: false,
    });
    this.canvas.addEventListener("touchcancel", this.onTouchEnd, {
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
    this.canvas.removeEventListener("wheel", this.onWheel);
    this.canvas.removeEventListener("touchstart", this.onTouchStart);
    this.canvas.removeEventListener("touchmove", this.onTouchMove);
    this.canvas.removeEventListener("touchend", this.onTouchEnd);
    this.canvas.removeEventListener("touchcancel", this.onTouchEnd);
    document.removeEventListener("pointerlockchange", this.onPointerLockChange);
  }

  async start() {
    this.statusCb({ state: "connecting" });
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
    this.sendSetEncodings([RFB_ENCODING_ZLIB, RFB_ENCODING_RAW]);
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
      this.statusCb({
        state: "error",
        error: e instanceof Error ? e.message : String(e),
      });
      this.q.close();
    }
  }

  private async handleFramebufferUpdate() {
    // padding
    await this.readU8();
    const rects = await this.readU16();
    if (!this.imageData) return;

    for (let i = 0; i < rects; i++) {
      const x = await this.readU16();
      const y = await this.readU16();
      const w = await this.readU16();
      const h = await this.readU16();
      const enc = await this.readI32();

      const bytesPerPixel = this.bytesPerPixel();
      const expectedLen = w * h * bytesPerPixel;
      if (enc === RFB_ENCODING_RAW) {
        if (expectedLen === 0) continue;
        const raw = await this.q.readExactly(expectedLen);
        this.blitRaw(x, y, w, h, raw, bytesPerPixel);
        continue;
      }

      if (enc === RFB_ENCODING_ZLIB) {
        const zlen = await this.readU32();
        if (expectedLen === 0 && zlen === 0) continue;
        const zdata = await this.q.readExactly(zlen);
        const inflated = this.inflateZlibRect(zdata, expectedLen);
        this.blitRaw(x, y, w, h, inflated, bytesPerPixel);
        continue;
      }

      throw new Error(`Unsupported encoding: ${enc}`);
    }

    this.presentFrameSoon();
  }

  private presentFrameSoon() {
    if (this.framePresentPending || !this.imageData) return;
    this.framePresentPending = true;
    const present = () => {
      this.framePresentPending = false;
      if (!this.imageData) return;
      this.ctx.putImageData(this.imageData, 0, 0);
    };
    if (typeof requestAnimationFrame === "function") {
      requestAnimationFrame(present);
    } else {
      setTimeout(present, 0);
    }
  }

  private ensureZlibStream() {
    if (this.zlibStream) return;
    this.zlibStream = new Unzlib((chunk) => {
      if (!this.zlibTarget) return;
      const next = this.zlibTargetOff + chunk.length;
      if (next > this.zlibTarget.length) {
        throw new Error(
          `Zlib rect overflow: got ${next}, expected ${this.zlibTarget.length}`,
        );
      }
      this.zlibTarget.set(chunk, this.zlibTargetOff);
      this.zlibTargetOff = next;
    });
  }

  private inflateZlibRect(data: Uint8Array, expectedLen: number): Uint8Array {
    if (expectedLen < 0) {
      throw new Error(`Invalid zlib rect length: ${expectedLen}`);
    }
    this.ensureZlibStream();
    const out = new Uint8Array(expectedLen);
    this.zlibTarget = out;
    this.zlibTargetOff = 0;
    this.zlibStream?.push(data, false);

    const got = this.zlibTargetOff;
    this.zlibTarget = null;

    if (got !== expectedLen) {
      throw new Error(
        `Zlib rect size mismatch: got ${got}, expected ${expectedLen}`,
      );
    }

    return out;
  }

  private bytesPerPixel(): number {
    const pf = this.pf;
    if (!pf) return 4;
    return Math.max(1, Math.floor(pf.bitsPerPixel / 8));
  }

  private isFastBgrx32(pf: PixelFormat | null): boolean {
    return !!(
      pf &&
      pf.bitsPerPixel === 32 &&
      pf.bigEndian === 0 &&
      pf.trueColor === 1 &&
      pf.redMax === 255 &&
      pf.greenMax === 255 &&
      pf.blueMax === 255 &&
      pf.redShift === 16 &&
      pf.greenShift === 8 &&
      pf.blueShift === 0
    );
  }

  private isFastRgbx32(pf: PixelFormat | null): boolean {
    return !!(
      pf &&
      pf.bitsPerPixel === 32 &&
      pf.bigEndian === 0 &&
      pf.trueColor === 1 &&
      pf.redMax === 255 &&
      pf.greenMax === 255 &&
      pf.blueMax === 255 &&
      pf.redShift === 0 &&
      pf.greenShift === 8 &&
      pf.blueShift === 16
    );
  }

  private blitRaw(
    x: number,
    y: number,
    w: number,
    h: number,
    raw: Uint8Array,
    bytesPerPixel: number,
  ) {
    const pf = this.pf;
    if (!this.imageData || !pf) return;
    if (bytesPerPixel === 4 && this.isFastBgrx32(pf)) {
      this.blitRaw32leBgrx(x, y, w, h, raw);
      return;
    }
    if (bytesPerPixel === 4 && this.isFastRgbx32(pf)) {
      this.blitRaw32leRgbx(x, y, w, h, raw);
      return;
    }
    if (!pf.trueColor) {
      throw new Error("RFB trueColor=false not supported");
    }

    const dst = this.imageData.data;
    const fbW = this.fbWidth;
    const rMul = pf.redMax === 255 ? 1 : 255 / pf.redMax;
    const gMul = pf.greenMax === 255 ? 1 : 255 / pf.greenMax;
    const bMul = pf.blueMax === 255 ? 1 : 255 / pf.blueMax;

    let srcOff = 0;
    for (let row = 0; row < h; row++) {
      let dstOff = ((y + row) * fbW + x) * 4;
      for (let col = 0; col < w; col++) {
        let pixel = 0;
        if (pf.bigEndian) {
          for (let i = 0; i < bytesPerPixel; i++) {
            pixel = (pixel << 8) | raw[srcOff + i]!;
          }
        } else {
          for (let i = 0; i < bytesPerPixel; i++) {
            pixel |= raw[srcOff + i]! << (8 * i);
          }
        }

        const rVal = (pixel >> pf.redShift) & pf.redMax;
        const gVal = (pixel >> pf.greenShift) & pf.greenMax;
        const bVal = (pixel >> pf.blueShift) & pf.blueMax;
        dst[dstOff + 0] = rMul === 1 ? rVal : Math.round(rVal * rMul);
        dst[dstOff + 1] = gMul === 1 ? gVal : Math.round(gVal * gMul);
        dst[dstOff + 2] = bMul === 1 ? bVal : Math.round(bVal * bMul);
        dst[dstOff + 3] = 255;

        srcOff += bytesPerPixel;
        dstOff += 4;
      }
    }
  }

  private blitRaw32leBgrx(
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

  private blitRaw32leRgbx(
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
        const r = raw[srcOff + 0]!;
        const g = raw[srcOff + 1]!;
        const b = raw[srcOff + 2]!;
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
    if (ev.repeat && ev.key !== "Backspace") return;
    if (ev.key === "Enter") {
      if (this.pendingCommandChars > 0) {
        this.onCommand?.();
      }
      this.pendingCommandChars = 0;
    } else if (ev.key === "Backspace") {
      this.pendingCommandChars = Math.max(0, this.pendingCommandChars - 1);
    } else if (ev.key === "Escape") {
      this.pendingCommandChars = 0;
    } else if (ev.key.length === 1 && !ev.ctrlKey && !ev.metaKey) {
      this.pendingCommandChars += 1;
    }
    const keysym = this.keysymFromEvent(ev);
    if (keysym == null) return;
    ev.preventDefault();
    if (keysym === KEYSYM.Control) {
      this.ctrlDown = true;
    } else if (ev.ctrlKey && !this.ctrlDown) {
      this.sendKeyEvent(true, KEYSYM.Control);
      this.ctrlSynth = true;
    }
    this.sendKeyEvent(true, keysym);
  };

  private onKeyUp = (ev: KeyboardEvent) => {
    const keysym = this.keysymFromEvent(ev);
    if (keysym == null) return;
    ev.preventDefault();
    this.sendKeyEvent(false, keysym);
    if (keysym === KEYSYM.Control) {
      this.ctrlDown = false;
    } else if (this.ctrlSynth && !this.ctrlDown) {
      this.sendKeyEvent(false, KEYSYM.Control);
      this.ctrlSynth = false;
    }
  };

  private keysymFromEvent(ev: KeyboardEvent): number | null {
    const k = ev.key;
    if (ev.code === "Backspace") return KEYSYM.BackSpace;
    if (k.length === 1) return k.codePointAt(0) ?? null;
    if (k in KEYSYM) return (KEYSYM as Record<string, number>)[k] ?? null;
    // common aliases
    if (k === "Esc") return KEYSYM.Escape;
    if (k === "Backspace") return KEYSYM.BackSpace;
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

  private touchPos(touch: Touch): { x: number; y: number } {
    const r = this.contentRect();
    const sx = this.fbWidth / r.width;
    const sy = this.fbHeight / r.height;
    const x = Math.max(
      0,
      Math.min(this.fbWidth - 1, Math.floor((touch.clientX - r.left) * sx)),
    );
    const y = Math.max(
      0,
      Math.min(this.fbHeight - 1, Math.floor((touch.clientY - r.top) * sy)),
    );
    return { x, y };
  }

  private touchById(list: TouchList, id: number | null): Touch | null {
    if (list.length === 0) return null;
    if (id == null) return list.item(0);
    for (let i = 0; i < list.length; i++) {
      const t = list.item(i);
      if (t && t.identifier === id) return t;
    }
    return null;
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

  private focusKeyboard() {
    this.keyboardInput?.focus();
  }

  private onCanvasClick = (ev: MouseEvent) => {
    this.focusKeyboard();
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

  private onTouchStart = (ev: TouchEvent) => {
    if (ev.touches.length === 0 && ev.changedTouches.length === 0) return;
    ev.preventDefault();
    this.focusKeyboard();
    if (this.touchId == null) {
      const first = ev.changedTouches[0] ?? ev.touches[0];
      if (!first) return;
      this.touchId = first.identifier;
    }
    const touch =
      this.touchById(ev.touches, this.touchId) ??
      this.touchById(ev.changedTouches, this.touchId);
    if (!touch) return;
    const p = this.touchPos(touch);
    this.lastAbsX = p.x;
    this.lastAbsY = p.y;
    this.sendPointerEvent(1, p.x, p.y);
  };

  private onTouchMove = (ev: TouchEvent) => {
    if (this.touchId == null) return;
    const touch = this.touchById(ev.touches, this.touchId);
    if (!touch) return;
    ev.preventDefault();
    const p = this.touchPos(touch);
    this.lastAbsX = p.x;
    this.lastAbsY = p.y;
    this.sendPointerEvent(1, p.x, p.y);
  };

  private onTouchEnd = (ev: TouchEvent) => {
    if (this.touchId == null) return;
    const touch = this.touchById(ev.changedTouches, this.touchId);
    if (!touch) return;
    ev.preventDefault();
    const p = this.touchPos(touch);
    this.lastAbsX = p.x;
    this.lastAbsY = p.y;
    this.sendPointerEvent(0, p.x, p.y);
    this.touchId = null;
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

  private onWheel = (ev: WheelEvent) => {
    ev.preventDefault();
    if (ev.deltaY === 0) return;
    const mask = ev.deltaY < 0 ? 8 : 16;
    if (document.pointerLockElement === this.canvas) {
      const x = Math.floor(this.plX);
      const y = Math.floor(this.plY);
      this.sendPointerEvent(mask, x, y);
      this.sendPointerEvent(0, x, y);
      return;
    }
    const p = this.pointerPos(ev);
    this.sendPointerEvent(mask, p.x, p.y);
    this.sendPointerEvent(0, p.x, p.y);
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
    const ver = new TextDecoder().decode(await this.q.readExactly(12));
    if (!ver.startsWith("RFB ")) throw new Error(`bad RFB version: ${ver}`);
    this.sendAscii(RFB_VERSION_38);
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
  }

  private async readServerInit(): Promise<ServerInit> {
    const width = await this.readU16();
    const height = await this.readU16();
    const pixelFormat = await this.readPixelFormat();
    const nameLen = await this.readU32();
    const name = new TextDecoder().decode(await this.q.readExactly(nameLen));
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
    const pf: PixelFormat = {
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
    };
    this.pf = pf;
    this.sendSetPixelFormat(pf);
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
    const out = copy as Uint8Array<ArrayBuffer>;
    if (
      this.sendQueueHead < this.sendQueue.length ||
      this.dc.bufferedAmount > this.bufferedHigh
    ) {
      this.sendQueue.push(out);
      return;
    }
    this.dc.send(out);
    this.flushSendQueue();
  }

  private flushSendQueue() {
    if (this.dc.readyState !== "open") return;
    while (
      this.sendQueueHead < this.sendQueue.length &&
      this.dc.bufferedAmount <= this.bufferedLow
    ) {
      const next = this.sendQueue[this.sendQueueHead];
      this.sendQueueHead += 1;
      if (!next) break;
      this.dc.send(next);
    }
    if (
      this.sendQueueHead > 64 &&
      this.sendQueueHead * 2 > this.sendQueue.length
    ) {
      this.sendQueue = this.sendQueue.slice(this.sendQueueHead);
      this.sendQueueHead = 0;
    }
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
