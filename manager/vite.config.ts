import { defineConfig } from "vite";

// The Tauri shell is the only consumer, so there is no dev/prod branching on
// browser support: WebView2 Evergreen ships current Chromium, and transpiling
// down to Safari-era syntax would cost bundle size for nobody.
//
// `strictPort` matters more than it looks: tauri.conf.json pins devUrl to 1420,
// and a Vite that silently moves to 1421 opens the window on a blank page that
// looks like an app bug rather than a taken port.
//
// sourcemap + minify together: the bundle stays small enough to be
// irrelevant inside the installer, and a WebView2 devtools stack trace still
// names real functions when a user reports something from an installed tree.
export default defineConfig({
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**"] },
  },
  envPrefix: ["VITE_", "TAURI_"],
  build: {
    target: "chrome110",
    // `true`, not "esbuild": Vite 8 no longer bundles esbuild (it uses oxc via
    // rolldown), and naming esbuild makes the build fail with "Cannot find package
    // 'esbuild'" unless it is installed as a separate dependency we do not need.
    minify: true,
    sourcemap: true,
  },
});
