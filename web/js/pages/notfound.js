/* The page of an address that leads nowhere. Served with status 404 for any unknown path, so its files are named from the root. */
(() => {
  'use strict';
  const { html } = MQ;
  MQ.page({
    id: '', title: 'Not found',
    render(root) {
      root.innerHTML = String(html`<div style="display:grid;place-items:center;min-block-size:60dvh">${MQ.empty('Page not found', '', 'search', html`<a class="mq-btn mq-btn--sm" href="${MQ.to('index')}">Go to Home</a>`)}</div>`);
    },
  });
})();
