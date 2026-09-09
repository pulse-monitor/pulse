import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'

export default defineConfig({
  plugins: [react(), tailwindcss()],
  build: {
    // 首屏预算 180 KB gzip。
    // 超了就 warn，好在 CI 里看得见。
    chunkSizeWarningLimit: 600,
    rollupOptions: {
      output: {
        // 地球与图表都不在首屏关键路径上，拆出去让首屏更小
        manualChunks: {
          // 图表不在首屏关键路径上，拆出去让首屏更小。
          // 地图是自己写的 SVG 投影，没有额外依赖
          chart: ['uplot'],
        },
      },
    },
  },
  server: {
    // 开发时把 API 代理到本地面板，免去 CORS 与改配置
    proxy: {
      '/api': { target: 'http://127.0.0.1:25774', ws: true, changeOrigin: true },
    },
  },
  test: { environment: 'node' },
})
