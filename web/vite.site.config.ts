import { resolve } from 'node:path';
import react from '@vitejs/plugin-react';
import { defineConfig } from 'vite';

export default defineConfig({
  plugins: [react()],
  server: { host: '127.0.0.1', port: 5174, strictPort: true, open: false },
  build: {
    outDir: 'dist-site-preview',
    target: 'es2022',
    assetsInlineLimit: 0,
    rollupOptions: { input: resolve(import.meta.dirname, 'design.html') },
  },
});
