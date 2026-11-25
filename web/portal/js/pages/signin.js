/* Sign in: a centered card with the one way in, Plex. No shell. In the app the button starts the
   plex.tv PIN flow; the preview goes straight to Discover. */
(() => {
  'use strict';
  const { html, icon } = MQ;
  const P = PORTAL;

  P.page({
    id: 'signin', title: 'Sign in', bare: true,
    async render(root) {
      const name = P.me.portal || 'Singularr Requests';
      root.classList.add('pt-page--bare');
      root.innerHTML = String(html`<div class="pt-signin"><div class="pt-signin-card">
        ${name === 'Singularr Requests' ? html`<div><h1 class="pt-signin-lockup">${MQ.mark('requests', name)}</h1>` : html`${MQ.mark('horizon')}<div><h1>${name}</h1>`}<p class="mt-3">Ask for movies and shows${P.me.server ? html` on <b>${P.me.server}</b>` : ''}, and follow them until they are ready to play.</p></div>
        <button type="button" class="mq-btn mq-btn--primary" data-signin>Sign in with Plex</button>
        <p class="pt-signin-who">For people the Plex server is shared with.</p>
        <hr>
        <small>Signing in keeps your Plex name and id to remember your requests. Your Plex password is never seen or stored.</small>
      </div></div>`);
      root.querySelector('[data-signin]').addEventListener('click', e => {
        const b = e.currentTarget; b.disabled = true; b.textContent = 'Waiting for Plex';
        setTimeout(() => { location.href = 'index.html'; }, 700);
      });
    },
  });
})();
