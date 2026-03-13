import { execSync } from 'node:child_process';

import tailwindcss from '@tailwindcss/vite';
import { defineConfig } from 'vite-plus';
import { sveltekit } from '@sveltejs/kit/vite';

const commitHash = execSync('git rev-parse --short HEAD').toString().trim();

export default defineConfig({
  define: {
    __APP_VERSION__: JSON.stringify('0.1.0-' + commitHash),
  },

  plugins: [tailwindcss(), sveltekit()],

  server: {
    proxy: {
      '/api': { target: 'http://localhost:8080', changeOrigin: true },
    },
  },
  preview: {
    proxy: {
      '/api': { target: 'http://localhost:8080', changeOrigin: true },
    },
  },

  lint: {
    ignorePatterns: [
      '.svelte-kit/**',
      '.venv/**',
      'build/**',
      'node_modules/**',
      'crates/**',
      '**/target/**',
    ],
  },

  fmt: {
    ignorePatterns: [
      '.cargo/**',
      '.github/**',
      'build/**',
      'node_modules/**',
      'crates/**',
      '**/target/**',
    ],
    trailingComma: 'all',
    singleQuote: true,
    semi: true,
    printWidth: 80,
  },

  test: {
    include: ['src/**/*.test.ts', 'src/**/*.spec.ts'],
  },

  staged: {
    '*': 'vp check --fix',
  },
});
