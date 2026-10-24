/**
 * 构建 `@inkdsh/ui-chat` 的浏览器半包。
 *
 * 输出格式对齐官方 tsdown client bundle：调用
 * `window.__ModuleLoader__.load({ id, factory: (require) => ... })`。
 * React、React DOM、官方 primitives/store 与 @tanstack/react-virtual 走
 * loader 的模块表（external）；纯工具包与 schemastery 内联进 bundle。
 *
 * CSS Modules 使用 lightningcss 生成哈希后的 class map，并在工厂执行时注入
 * 一个带 data-plugin-css 的 <style>，避免不同模块的 `.root`/`.body` 冲突。
 */
import { readFile } from 'node:fs/promises';
import { dirname, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { build } from 'esbuild';
import { transform } from 'lightningcss';

const packageDir = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const pluginId = '@inkdsh/ui-chat';

/** 由 loader 模块表提供的运行时依赖。 */
const externals = [
  '@deepseek-ai/dsh-client-store',
  '@deepseek-ai/dsh-client-ui-primitives',
  '@tanstack/react-virtual',
  'react',
  'react-dom',
  'react/jsx-runtime',
];

/**
 * 把 `*.module.css` 编译成「样式注入 + 哈希 class map」的 JS 模块。
 *
 * @returns esbuild 插件对象。
 */
function cssModulesPlugin() {
  return {
    name: 'inkdsh-css-modules',
    setup(buildApi) {
      buildApi.onLoad({ filter: /\.module\.css$/ }, async (args) => {
        const source = await readFile(args.path, 'utf8');
        const relativePath = relative(packageDir, args.path).replaceAll('\\', '/');
        const result = transform({
          filename: relativePath,
          code: Buffer.from(source),
          cssModules: { pattern: '[hash]_[local]' },
        });
        const css = result.code.toString('utf8');
        const classMap = result.exports ?? {};
        const tagId = `${pluginId}/${relativePath}`;
        const contents = [
          `const css = ${JSON.stringify(css)};`,
          `const tagId = ${JSON.stringify(tagId)};`,
          "if (typeof document !== 'undefined' && document.querySelector('style[data-plugin-css=' + JSON.stringify(tagId) + ']') === null) {",
          "  const tag = document.createElement('style');",
          `  tag.dataset.plugin = ${JSON.stringify(pluginId)};`,
          '  tag.dataset.pluginCss = tagId;',
          '  tag.textContent = css;',
          '  document.head.appendChild(tag);',
          '}',
          `export default ${JSON.stringify(classMap)};`,
        ].join('\n');
        return { loader: 'js', contents, resolveDir: dirname(args.path) };
      });
      buildApi.onLoad({ filter: /\.png$/ }, async (args) => {
        const bytes = await readFile(args.path);
        return {
          loader: 'dataurl',
          contents: bytes,
          resolveDir: dirname(args.path),
        };
      });
    },
  };
}

const result = await build({
  absWorkingDir: packageDir,
  entryPoints: [resolve(packageDir, 'src/client.ts')],
  bundle: true,
  write: false,
  format: 'cjs',
  platform: 'browser',
  target: ['chrome110'],
  jsx: 'automatic',
  external: externals,
  loader: { '.png': 'dataurl' },
  plugins: [cssModulesPlugin()],
  define: { 'process.env.NODE_ENV': '"production"' },
  legalComments: 'none',
  logLevel: 'info',
});

const body = result.outputFiles[0]?.text;
if (!body) throw new Error('esbuild 没有产出 client bundle');

const wrapped = [
  'window.__ModuleLoader__.load({',
  `  id: ${JSON.stringify(pluginId)},`,
  '  factory: (require) => {',
  '    var module = { exports: {} };',
  '    var exports = module.exports;',
  body,
  '    return module.exports;',
  '  },',
  '});',
  '',
].join('\n');

const outputPath = resolve(packageDir, 'lib/client.js');
await import('node:fs/promises').then(({ writeFile, mkdir }) =>
  mkdir(dirname(outputPath), { recursive: true }).then(() => writeFile(outputPath, wrapped, 'utf8')),
);
console.info(`[inkdsh/ui-chat] wrote ${relative(packageDir, outputPath)} (${wrapped.length} bytes)`);
