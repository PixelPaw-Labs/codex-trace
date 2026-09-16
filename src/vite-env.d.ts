/// <reference types="vite/client" />

/** Served by the `codextrace-api-token` Vite plugin (see `vite.config.ts`):
 * the `web-ui` client's credential in dev/web mode, and always `""` in a
 * production build. */
declare module "virtual:codextrace-api-token" {
  const credential: string;
  export default credential;
}
