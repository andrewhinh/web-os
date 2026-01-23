declare module "svelte-vnc" {
  import type { SvelteComponent } from "svelte";

  export interface NoVNCProps {
    host?: string | null;
    port?: string | null;
    encrypt?: boolean | null;
    password?: string | null;
    autoconnect?: boolean | null;
    reconnect?: boolean | null;
    reconnect_delay?: number | null;
    quality?: number | null;
    compression?: number | null;
    resize?: string | null;
    view_only?: boolean | null;
    logging?: boolean | null;
    shared?: boolean | null;
    bell?: boolean | null;
    show_dot?: boolean | null;
    path?: string | null;
    repeaterID?: string | null;
    controlbar_pos?: string | null;
    view_clip?: boolean | null;
    embedded_server?: boolean | null;
    username?: string | null;
    brightness?: number;
    isFullscreen?: boolean;
    class?: string;
    style?: string;
  }

  export default class NoVNC extends SvelteComponent<NoVNCProps> {}
}
