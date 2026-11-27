import { defineConfig } from "vite"
import { gui } from "@opencode/plugin/gui/vite"

// The Desktop GUI must be one prebuilt CommonJS file; the preset inlines
// dynamic imports and keeps OpenCode's shared modules external.
export default defineConfig({ plugins: [gui()] })
