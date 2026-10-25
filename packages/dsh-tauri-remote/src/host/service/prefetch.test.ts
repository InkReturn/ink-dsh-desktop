import { describe, expect, it } from 'vitest'
import { extractPrefetchUrls } from './prefetch'

describe('remote prefetch plan', () => {
  it('collects same-origin cacheable assets and versioned plugin bundles', () => {
    const html = `<!doctype html><base href="./">
      <script src="./assets/index-abc.js" type="module"></script>
      <link rel="stylesheet" href="/assets/index-abc.css">
      <link rel="icon" href="/favicon.svg">
      <link rel="manifest" href="/manifest.webmanifest">
      <link rel="modulepreload" href="./assets/vendor.js">
      <script src="/plugins/??@deepseek-ai/a/client.js,@local/b/client.js&amp;rev=123"></script>
      <script src="https://cdn.example.com/external.js"></script>
      <script src="/api/session"></script>
      <script src="./assets/index-abc.js"></script>`
    expect(extractPrefetchUrls(html, 'http://127.0.0.1:4100/')).toEqual([
      'http://127.0.0.1:4100/assets/index-abc.js',
      'http://127.0.0.1:4100/assets/index-abc.css',
      'http://127.0.0.1:4100/favicon.svg',
      'http://127.0.0.1:4100/manifest.webmanifest',
      'http://127.0.0.1:4100/assets/vendor.js',
      'http://127.0.0.1:4100/plugins/??@deepseek-ai/a/client.js,@local/b/client.js&rev=123',
    ])
  })

  it('ignores HTML comments, inline scripts and absolute query-free plugin URLs', () => {
    const html = `<!-- <script src="/assets/old.js"></script> -->
      <script>console.log('inline')</script>
      <script src="/plugins/client.js"></script>
      <link href="/assets/app.css" rel="stylesheet">`
    expect(extractPrefetchUrls(html, 'http://127.0.0.1:4100/root/')).toEqual([
      'http://127.0.0.1:4100/assets/app.css',
    ])
  })
})
