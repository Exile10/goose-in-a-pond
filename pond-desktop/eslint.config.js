// ESLint for the dashboard (src/), the Electron shell (electron/) and the Playwright suites
// (tests/). `npm run lint` runs it and the frontend CI job fails on any error. Every enabled rule
// is an error; docs/CONTRIBUTING.md lists the ones that are off.
import js from '@eslint/js'
import { defineConfig, globalIgnores } from 'eslint/config'
import reactHooks from 'eslint-plugin-react-hooks'
import reactRefresh from 'eslint-plugin-react-refresh'
import globals from 'globals'
import tseslint from 'typescript-eslint'

export default defineConfig([
  // Build output, generated reports, the staged sidecar and static assets served as they are.
  globalIgnores([
    'dist/',
    'dist-electron/',
    'release/',
    'resources/',
    'coverage/',
    'playwright-report/',
    'test-results/',
    'kiosk-screenshots/',
    'src-tauri/',
    'public/',
  ]),

  {
    files: ['**/*.{ts,tsx,js,mjs,cjs}'],
    extends: [js.configs.recommended, tseslint.configs.recommended],
    rules: {
      // Destructuring a key out beside `...rest` is how a copy without that key is made, and a
      // parameter an interface requires but one implementation does not read is named `_name`.
      '@typescript-eslint/no-unused-vars': ['error', { ignoreRestSiblings: true, argsIgnorePattern: '^_' }],
    },
  },

  // The renderer: React in a browser context.
  {
    files: ['src/**/*.{ts,tsx}'],
    languageOptions: { globals: globals.browser },
    plugins: { 'react-hooks': reactHooks },
    extends: [reactRefresh.configs.vite],
    rules: {
      // Only the two classic hook rules. The React Compiler rules that the plugin's recommended set
      // adds would report 69 existing hits (56 of them set-state-in-effect); not enabled yet.
      'react-hooks/rules-of-hooks': 'error',
      'react-hooks/exhaustive-deps': 'error',
      // Fast Refresh falls back to a full reload for a file that exports a component next to a
      // hook, context or helper, or that has no exports at all. Helpers, hooks and contexts live in
      // a sibling module instead (`useConfirm.ts` beside ConfirmDialog.tsx).
      'react-refresh/only-export-components': ['error', { allowConstantExport: true, allowCompoundComponents: true }],
    },
  },

  // A test's mock stream that fails before its first frame is an async generator that only
  // throws: it has to be a generator to have the type, and it has nothing to yield.
  {
    files: ['src/**/*.test.{ts,tsx}'],
    rules: { 'require-yield': 'off' },
  },

  // The Electron main process and preload, the Playwright suites and the tooling at the package
  // root all run in Node. The Playwright suites also hand functions to the page, which run there.
  {
    files: ['electron/**/*.ts', '*.{ts,js,mjs}', 'scripts/**/*.mjs'],
    languageOptions: { globals: globals.node },
  },
  {
    files: ['tests/**/*.ts'],
    languageOptions: { globals: { ...globals.node, ...globals.browser } },
  },
])
