'use strict';

// ---------- helpers ----------
const $ = (s, el = document) => el.querySelector(s);
const $$ = (s, el = document) => [...el.querySelectorAll(s)];
const call = (name, ...args) => window.go.main.App[name](...args);
const esc = (s) => String(s ?? '').replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
const debounce = (fn, ms) => { let t; return (...a) => { clearTimeout(t); t = setTimeout(() => fn(...a), ms); }; };
const TERMINAL = new Set(['done', 'partial', 'failed', 'canceled', 'interrupted', 'skipped']);

const I = {
  up: '<svg viewBox="0 0 20 20"><path d="M10 15.5V4.5m0 0L5.5 9M10 4.5L14.5 9" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/></svg>',
  down: '<svg viewBox="0 0 20 20"><path d="M10 4.5v11m0 0L5.5 11m4.5 4.5l4.5-4.5" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/></svg>',
  folder: '<svg viewBox="0 0 20 20"><path d="M3 6.2c0-.7.5-1.2 1.2-1.2h3.4l1.6 1.7h6.6c.7 0 1.2.5 1.2 1.2v6.9c0 .7-.5 1.2-1.2 1.2H4.2c-.7 0-1.2-.5-1.2-1.2z" fill="none" stroke="currentColor" stroke-width="1.4"/></svg>',
  project: '<svg viewBox="0 0 20 20"><rect x="3" y="4" width="14" height="12" rx="2" fill="none" stroke="currentColor" stroke-width="1.4"/><path d="M3 8h14" stroke="currentColor" stroke-width="1.4"/></svg>',
  team: '<svg viewBox="0 0 20 20"><circle cx="8" cy="7.5" r="2.6" fill="none" stroke="currentColor" stroke-width="1.4"/><path d="M3.5 15.5c.6-2.4 2.3-3.6 4.5-3.6s3.9 1.2 4.5 3.6M13 5.2a2.4 2.4 0 0 1 0 4.6M14.5 11.9c1.3.5 2.1 1.6 2.4 3.6" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"/></svg>',
  x: '<svg viewBox="0 0 20 20"><path d="M6 6l8 8M14 6l-8 8" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/></svg>',
  retry: '<svg viewBox="0 0 20 20"><path d="M15.5 10a5.5 5.5 0 1 1-1.6-3.9M15.5 4v3.4h-3.4" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/></svg>',
  reveal: '<svg viewBox="0 0 20 20"><path d="M3 6.2c0-.7.5-1.2 1.2-1.2h3.4l1.6 1.7h6.6c.7 0 1.2.5 1.2 1.2v6.9c0 .7-.5 1.2-1.2 1.2H4.2c-.7 0-1.2-.5-1.2-1.2z" fill="none" stroke="currentColor" stroke-width="1.4"/></svg>',
  chevron: '<svg viewBox="0 0 20 20"><path d="M7 8l3 3 3-3" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/></svg>',
  chevronUp: '<svg viewBox="0 0 20 20"><path d="M7 12l3-3 3 3" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/></svg>',
  open: '<svg viewBox="0 0 20 20"><path d="M8 5H5.5A1.5 1.5 0 0 0 4 6.5v8A1.5 1.5 0 0 0 5.5 16h8a1.5 1.5 0 0 0 1.5-1.5V12M11 4h5v5M16 4l-7 7" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/></svg>',
  copy: '<svg viewBox="0 0 20 20"><rect x="7" y="7" width="9" height="9" rx="1.5" fill="none" stroke="currentColor" stroke-width="1.4"/><path d="M13 7V5.5A1.5 1.5 0 0 0 11.5 4h-6A1.5 1.5 0 0 0 4 5.5v6A1.5 1.5 0 0 0 5.5 13H7" fill="none" stroke="currentColor" stroke-width="1.4"/></svg>',
  pencil: '<svg viewBox="0 0 20 20"><path d="M12.8 4.7l2.5 2.5M4 16l.7-3.2 8.6-8.6a1.2 1.2 0 0 1 1.7 0l.8.8a1.2 1.2 0 0 1 0 1.7l-8.6 8.6z" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linejoin="round"/></svg>',
  sync: '<svg viewBox="0 0 20 20"><path d="M6.5 15V5m0 0L3.8 7.7M6.5 5l2.7 2.7M13.5 5v10m0 0l-2.7-2.7m2.7 2.7l2.7-2.7" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/></svg>',
  external: '<svg viewBox="0 0 20 20" width="12" height="12"><path d="M8 5H5.5A1.5 1.5 0 0 0 4 6.5v8A1.5 1.5 0 0 0 5.5 16h8a1.5 1.5 0 0 0 1.5-1.5V12M11 4h5v5M16 4l-7 7" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/></svg>',
};

function fmtBytes(n) {
  if (!n || n < 0) return '0 B';
  const u = ['B', 'KB', 'MB', 'GB', 'TB', 'PB'];
  let i = 0, v = n;
  while (v >= 1000 && i < u.length - 1) { v /= 1000; i++; }
  const s = i === 0 ? v.toFixed(0) : v >= 100 ? v.toFixed(0) : v >= 10 ? v.toFixed(1) : v.toFixed(2);
  return `${s} ${u[i]}`;
}
const fmtSpeed = (bps) => (!bps || bps < 1 ? '—' : `${fmtBytes(bps)}/s`);
const fmtMbit = (bps) => `${Math.round((bps * 8) / 1e6)} Mbit/s`;
function fmtDur(s) {
  if (!isFinite(s) || s <= 0) return '—';
  s = Math.round(s);
  const h = Math.floor(s / 3600), m = Math.floor((s % 3600) / 60), sec = s % 60;
  if (h) return `${h}h ${String(m).padStart(2, '0')}m`;
  if (m) return `${m}m ${String(sec).padStart(2, '0')}s`;
  return `${sec}s`;
}
function fmtDate(ms) {
  if (!ms) return '—';
  const d = new Date(ms), now = new Date();
  const time = d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
  const day = (x) => new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
  const diff = (day(now) - day(d)) / 86400000;
  if (diff === 0) return `Today, ${time}`;
  if (diff === 1) return `Yesterday, ${time}`;
  return `${d.toLocaleDateString([], { month: 'short', day: 'numeric', year: d.getFullYear() !== now.getFullYear() ? 'numeric' : undefined })}, ${time}`;
}
function shortPath(p) {
  const home = S.state?.home;
  if (home && p && (p === home || p.startsWith(home + '/'))) return '~' + p.slice(home.length);
  return p || '';
}
const baseName = (p) => (p || '').replace(/\/+$/, '').split('/').pop() || p;
const shortLink = (l) => (l || '').replace(/^https?:\/\//, '').replace(/\/$/, '');
const initials = (name) => (name || '?').split(/\s+/).filter(Boolean).slice(0, 2).map((w) => w[0].toUpperCase()).join('');

function statusLabel(status, kind) {
  return {
    queued: 'Queued', resolving: 'Preparing', running: kind === 'upload' ? 'Uploading' : kind === 'download' ? 'Downloading' : 'In progress',
    done: 'Completed', partial: 'Completed with errors', failed: 'Failed', canceled: 'Canceled',
    interrupted: 'Interrupted', skipped: 'Skipped',
  }[status] || status;
}
const statusHTML = (status, kind) => `<span class="status ${status}" title="${esc(statusLabel(status, kind))}"><span class="dot"></span><span class="trunc">${statusLabel(status, kind)}</span></span>`;

function toast(msg, kind = 'info', ms = 5000) {
  const el = document.createElement('div');
  el.className = `toast ${kind}`;
  el.innerHTML = `<span>${esc(msg)}</span>`;
  $('#toasts').appendChild(el);
  setTimeout(() => el.remove(), ms);
}
const errMsg = (e) => (e && e.message) || String(e);
// copyShare copies a public share link to a transfer's files; Frame.io makes
// the share on first use, so later copies give the same link.
async function copyShare(id, btn) {
  if (btn) btn.disabled = true;
  try {
    window.runtime.ClipboardSetText(await call('ShareLink', id));
    toast('Share link copied', 'ok', 2500);
  } catch (e) { toast(errMsg(e), 'err'); }
  if (btn) btn.disabled = false;
}

// ---------- state ----------
const S = {
  state: null,
  snap: { jobs: [], speed: 0, active_streams: 0, stream_limit: 16 },
  expanded: new Set(),
  filesFetchedAt: {},
  prevStatus: {},
  view: 'transfers',
  dlDest: '',
  linkInfo: null,
  linkSeq: 0,
  histKind: '',
  histExpanded: new Set(),
  pendingPaths: null,
  watchQueue: [], // folders from Finder's "Create Watch Folder in FioTransfer", set up one by one
  update: null,
};

// ---------- boot ----------
async function waitForBindings() {
  for (let i = 0; i < 200; i++) {
    if (window.go?.main?.App && window.runtime) return;
    await new Promise((r) => setTimeout(r, 25));
  }
  throw new Error('Wails bindings not available');
}

async function init() {
  await waitForBindings();
  bindUI();
  S.state = await call('GetState');
  S.dlDest = S.state.settings.download_dir;
  renderState();
  S.snap = await call('Snapshot');
  renderSnapshot();
  window.runtime.EventsOn('snapshot', (snap) => { S.snap = snap; renderSnapshot(); });
  window.runtime.EventsOn('files-dropped', onDrop);
  window.runtime.EventsOn('menu', onMenu);
  window.runtime.EventsOn('state-changed', async () => {
    S.state = await call('GetState');
    S.dlDest = S.state.settings.download_dir; // the menu bar's download panel can change it
    renderState();
  });
  window.runtime.EventsOn('update', (u) => { S.update = u; renderUpdate(); });
  S.update = await call('UpdateStatus');
  renderUpdate();
  loadWatches();
  setInterval(loadWatches, 2000);
  window.runtime.EventsOn('service', takeServiceRequests);
  takeServiceRequests(); // the app may have been opened from Finder for one
}
document.addEventListener('DOMContentLoaded', () => init().catch((e) => toast(errMsg(e), 'err', 15000)));

// ---------- wiring ----------
function bindUI() {
  $$('.nav-item').forEach((b) => b.addEventListener('click', () => showView(b.dataset.view)));
  document.addEventListener('click', (e) => {
    const a = e.target.closest('[data-url]');
    if (a) { e.preventDefault(); call('OpenURL', a.dataset.url); }
  });
  document.addEventListener('keydown', (e) => {
    if (e.key !== 'Escape') return;
    if (!$('#dest-modal').classList.contains('hidden')) closeDest();
    else if (!$('#watch-modal').classList.contains('hidden')) closeWatchEdit();
  });

  const inspectSoon = debounce(inspectLink, 450);
  $('#link').addEventListener('input', () => { S.linkInfo = null; updateDownloadBtn(); inspectSoon(); });
  $('#link').addEventListener('keydown', (e) => { if (e.key === 'Enter') startDownload(); });
  $('#passphrase').addEventListener('input', inspectSoon);
  $('#passphrase').addEventListener('keydown', (e) => { if (e.key === 'Enter') startDownload(); });
  $('#paste-btn').addEventListener('click', async () => {
    const t = (await call('ClipboardText')).trim();
    if (!t) return;
    $('#link').value = t;
    S.linkInfo = null;
    updateDownloadBtn();
    inspectLink();
  });
  $('#dl-dest').addEventListener('click', chooseDownloadDir);
  $('#download-btn').addEventListener('click', startDownload);
  $('#clear-btn').addEventListener('click', () => call('ClearFinished'));

  $('#history-q').addEventListener('input', debounce(loadHistory, 250));
  $$('#history-kind button').forEach((b) => b.addEventListener('click', () => {
    $$('#history-kind button').forEach((x) => x.classList.toggle('active', x === b));
    S.histKind = b.dataset.kind;
    loadHistory();
  }));
  $('#open-logs').addEventListener('click', () => call('OpenLogs'));
  $('#open-logs-2').addEventListener('click', () => call('OpenLogs'));
  $('#export-csv').addEventListener('click', async () => {
    try {
      const p = await call('ExportHistory');
      if (p) toast(`Exported to ${shortPath(p)}`, 'ok');
    } catch (e) { toast(errMsg(e), 'err'); }
  });

  $('#save-client').addEventListener('click', () => saveSettings({ client_id: $('#client-id').value.trim() }, 'Saved'));
  $('#client-id').addEventListener('keydown', (e) => { if (e.key === 'Enter') $('#save-client').click(); });
  $('#streams-range').addEventListener('input', (e) => { $('#streams-val').textContent = e.target.value; });
  $('#streams-range').addEventListener('change', (e) => saveSettings({ streams: +e.target.value }));
  $('#segment').addEventListener('change', (e) => saveSettings({ segment_mb: +e.target.value }));
  $('#open-login').addEventListener('change', async (e) => {
    try { S.state = await call('SetOpenAtLogin', e.target.checked); renderState(); } catch (err) { toast(errMsg(err), 'err'); }
  });
  $('#folder-logs').addEventListener('change', (e) => saveSettings({ no_folder_logs: !e.target.checked }));
  $('#auto-update').addEventListener('change', async (e) => {
    await saveSettings({ no_update_check: !e.target.checked });
    if (e.target.checked) S.update = await call('CheckForUpdate');
    renderUpdate();
  });
  $('#update-btn').addEventListener('click', async () => {
    if (S.update?.available) return installUpdate();
    S.update = await call('CheckForUpdate');
    renderUpdate();
  });
  $('#uc-btn').addEventListener('click', installUpdate);
  $('#change-default-dir').addEventListener('click', async () => {
    const p = await call('PickDownloadDir', S.state.settings.download_dir);
    if (p) await saveSettings({ download_dir: p });
  });
  $('#copy-redirect').addEventListener('click', (e) => {
    e.preventDefault();
    window.runtime.ClipboardSetText(S.state.redirect_uri);
    toast('Copied', 'ok', 2000);
  });

  $('#watch-add').addEventListener('click', addWatch);
  $('#we-close').addEventListener('click', closeWatchEdit);
  $('#we-cancel').addEventListener('click', closeWatchEdit);
  $('#we-save').addEventListener('click', saveWatchEdit);
  $$('#dest-dir button').forEach((b) => b.addEventListener('click', () => { D.watchDir = b.dataset.dir; renderDestTitle(); }));
  $$('#we-dir button').forEach((b) => b.addEventListener('click', () => { WE.dir = b.dataset.dir; renderWatchEdit(); }));
  $$('#dest-change button').forEach((b) => b.addEventListener('click', () => { D.watchChange = b.dataset.change; renderDestTitle(); }));
  $$('#we-change button').forEach((b) => b.addEventListener('click', () => { WE.change = b.dataset.change; renderWatchEdit(); }));
  $('#we-local-btn').addEventListener('click', async () => {
    const p = await call('PickWatchFolder');
    if (!p) return;
    WE.local = p;
    renderWatchEdit();
  });
  $('#we-dest-btn').addEventListener('click', () => openDest('pick', (d) => { Object.assign(WE, d); renderWatchEdit(); }));
  $('#we-requeue').addEventListener('click', async () => {
    await call('RequeueCanceled', WE.id);
    WE.canceled = 0;
    renderWatchEdit();
    loadWatches();
  });
  $('#dest-close').addEventListener('click', closeDest);
  $('#dest-cancel').addEventListener('click', closeDest);
  $('#dest-choose').addEventListener('click', chooseDest);
  $('#newfolder-btn').addEventListener('click', newFolder);
  $('#dest-q').addEventListener('input', onDestQuery);
  $('#dest-q').addEventListener('keydown', onDestKey);
  $('#newfolder-name').addEventListener('keydown', (e) => {
    if (e.key === 'Enter') newFolder();
    if (e.key === 'Escape') { e.stopPropagation(); $('#newfolder-name').classList.add('hidden'); }
  });
}

function showView(v) {
  S.view = v;
  $$('.nav-item').forEach((b) => b.classList.toggle('active', b.dataset.view === v));
  $$('.view').forEach((s) => s.classList.toggle('active', s.id === `view-${v}`));
  if (v === 'history') loadHistory();
  if (v === 'watch') loadWatches();
}

function onMenu(what) {
  if (what === 'update') { showView('settings'); $('#update-line').scrollIntoView({ block: 'nearest' }); return; }
  if (what === 'download') { showView('transfers'); $('#link').focus(); $('#link').select(); return; }
  if (what === 'upload-files') pickFiles();
  if (what === 'upload-folder') pickFolder();
}

// ---------- state rendering ----------
function renderState() {
  const st = S.state, s = st.settings;

  const acc = $('#account');
  acc.classList.remove('clickable');
  acc.onclick = null;
  if (st.logged_in) {
    const u = st.user || { name: 'Signed in', email: '' };
    acc.innerHTML = `<div class="avatar">${esc(initials(u.name))}</div>
      <div class="who"><span class="n">${esc(u.name)}</span><span class="e">${esc(u.email)}</span></div>`;
    acc.classList.add('clickable');
    acc.onclick = () => showView('settings');
  } else {
    acc.innerHTML = `<button class="btn" id="side-signin">Sign in to Frame.io</button>`;
    $('#side-signin').onclick = login;
  }

  renderUploadRow();
  $('#dl-dest-label').textContent = baseName(S.dlDest || s.download_dir);
  $('#dl-dest').title = `Save to ${S.dlDest || s.download_dir}`;

  $('#client-id').value = s.client_id || '';
  $('#redirect-uri').textContent = st.redirect_uri;
  $('#streams-range').value = s.streams;
  $('#streams-val').textContent = s.streams;
  const seg = $('#segment');
  if (![...seg.options].some((o) => o.value === String(s.segment_mb))) seg.insertAdjacentHTML('beforeend', `<option value="${s.segment_mb}">${s.segment_mb} MB</option>`);
  seg.value = String(s.segment_mb);
  $('#default-dir').textContent = shortPath(s.download_dir);
  $('#default-dir').title = s.download_dir;
  $('#folder-logs').checked = !s.no_folder_logs;
  $('#open-login').checked = !!st.open_at_login;
  $('#auto-update').checked = !s.no_update_check;
  $('#log-path').textContent = shortPath(st.log_file || st.log_dir);
  $('#about').textContent = `FioTransfer ${st.version}`;

  const row = $('#signin-row');
  if (st.logged_in) {
    row.innerHTML = `<div class="status-text">Signed in${st.user ? ` as <b>${esc(st.user.name)}</b> · ${esc(st.user.email)}` : ''}</div>
      <button class="btn" id="signout-btn">Sign out</button>`;
    $('#signout-btn').onclick = async () => { S.state = await call('Logout'); renderState(); };
    $('#setup-details').open = false;
  } else {
    row.innerHTML = `<div class="status-text">${s.client_id ? 'Not signed in' : 'Not set up'}</div>
      <button class="btn primary" id="signin-btn" ${s.client_id ? '' : 'disabled'}>Sign in with Adobe</button>`;
    $('#signin-btn').onclick = login;
    if (!s.client_id) $('#setup-details').open = true;
  }
}

function renderUploadRow() {
  const st = S.state, s = st.settings, body = $('#upload-body');
  if (!st.logged_in) {
    body.innerHTML = s.client_id
      ? `<div class="notice-line">Not signed in</div><button class="btn primary" id="cta-signin">Sign in</button>`
      : `<div class="notice-line">Not set up</div><button class="btn" id="cta-setup">Set up</button>`;
    if (s.client_id) $('#cta-signin').onclick = login;
    else $('#cta-setup').onclick = () => showView('settings');
    $('#drop-dest').textContent = '';
    return;
  }
  const label = s.last_folder_id ? s.last_folder_label : '';
  // Each part opens the picker at its level: the icon at the project list.
  const path = storedPath(s);
  const names = path ? path.slice(path.findIndex((x) => x.kind === 'project')).map((x) => x.name) : label ? label.split(' / ') : [];
  const crumbs = names.length ? names.map((n, k) => `<span class="seg" data-seg="${k}">${esc(n)}</span>`).join('<span class="sep">/</span>') : '<span class="ph">Choose destination</span>';
  body.innerHTML = `
    <button class="dest-pick" id="dest-btn" title="${esc(label || 'Choose destination')}"><span class="seg root" data-seg="-1" title="All projects">${I.project}</span><span class="path">${crumbs}</span><span class="chev">${I.chevron}</span></button>
    <button class="btn" id="pick-files">Files…</button>
    <button class="btn" id="pick-folder">Folder…</button>`;
  $('#dest-btn').onclick = (e) => {
    const seg = e.target.closest('[data-seg]');
    openDestAt(seg ? +seg.dataset.seg : null);
  };
  $('#pick-files').onclick = pickFiles;
  $('#pick-folder').onclick = pickFolder;
  $('#drop-dest').textContent = label;
}

async function saveSettings(patch, okMsg) {
  try {
    S.state = await call('SaveSettings', { ...S.state.settings, ...patch });
    if (patch.download_dir) S.dlDest = patch.download_dir;
    renderState();
    if (okMsg) toast(okMsg, 'ok', 2500);
  } catch (e) { toast(errMsg(e), 'err'); }
}

// Clicking a sign-in button again restarts sign-in (the backend cancels the
// attempt still waiting for the browser); only the latest attempt's result counts.
let loginSeq = 0;
async function login() {
  if (!S.state.settings.client_id) { showView('settings'); $('#client-id').focus(); return; }
  const seq = ++loginSeq;
  $$('#signin-btn, #cta-signin, #side-signin').forEach((b) => { b.textContent = 'Waiting for browser…'; b.title = 'Click to open the sign-in page again'; });
  let st;
  try {
    st = await call('Login');
  } catch (e) {
    if (seq !== loginSeq) return;
    toast(errMsg(e), 'err', 12000);
    st = await call('GetState');
  }
  if (seq !== loginSeq) return;
  S.state = st;
  renderState();
}

// ---------- updates ----------
function renderUpdate() {
  const u = S.update;
  if (!u) return;
  const dev = u.blocker === 'development build';
  const busy = u.state === 'downloading' || u.state === 'restarting';
  const progress = u.state === 'downloading' ? `Downloading… ${Math.floor((u.progress || 0) * 100)}%` : 'Restarting…';

  $('#update-card').classList.toggle('hidden', !u.available && !busy);
  $('.uc-title').textContent = busy ? 'Updating' : u.blocker ? 'New version' : 'Update available';
  $('#uc-sub').textContent = busy ? progress : `Version ${u.latest}`;
  $('#uc-btn').classList.toggle('hidden', busy);
  $('#uc-btn').textContent = u.blocker ? 'Download' : 'Update';

  $('#app-version').textContent = u.current;
  let line;
  if (dev) line = 'Development build';
  else if (busy) line = esc(progress);
  else if (u.state === 'checking') line = 'Checking…';
  else if (u.available) line = `${esc(u.latest)} available · <a data-url="${esc(u.url)}">What's new</a>${u.blocker ? ` · ${esc(u.blocker)}` : ''}`;
  else if (u.checked_at && !u.error) line = 'Up to date';
  else line = '';
  if (u.error) line += `${line ? ' · ' : ''}<span class="err-text">${esc(u.error)}</span>`;
  $('#update-line').innerHTML = line;
  $('#update-line').title = u.checked_at ? `Checked ${fmtDate(u.checked_at)}` : '';

  $('#auto-update').closest('.set-line').classList.toggle('hidden', dev);
  const btn = $('#update-btn');
  btn.classList.toggle('hidden', dev);
  btn.disabled = busy || u.state === 'checking';
  btn.classList.toggle('primary', !!u.available && !u.blocker);
  btn.textContent = u.available ? (u.blocker ? 'Download' : 'Update') : 'Check now';
}

async function installUpdate() {
  const u = S.update;
  if (!u?.available) return;
  if (u.blocker) return call('OpenURL', u.url); // can't replace itself here: get it from the release page
  try { await call('InstallUpdate'); } catch (e) { toast(errMsg(e), 'err'); }
}

// ---------- uploads ----------
async function pickFiles() {
  if (!ensureCanUpload()) return;
  const paths = await call('PickFiles');
  if (paths && paths.length) startUpload(paths);
}
async function pickFolder() {
  if (!ensureCanUpload()) return;
  const p = await call('PickFolder');
  if (p) startUpload([p]);
}
function ensureCanUpload() {
  if (S.state.logged_in) return true;
  showView(S.state.settings.client_id ? 'transfers' : 'settings');
  toast(S.state.settings.client_id ? 'Not signed in' : 'Uploads not set up', 'err');
  return false;
}
// Finder's right-click menu: uploads go through the destination picker (the
// window's destination bar isn't in view there); watch folders open set up.
async function takeServiceRequests() {
  const reqs = (await call('TakeServiceRequests')) || [];
  const up = reqs.filter((r) => r.kind === 'upload').flatMap((r) => r.paths);
  const watch = reqs.filter((r) => r.kind === 'watch').flatMap((r) => r.paths);
  if (!up.length && !watch.length) return;
  if (!ensureCanUpload()) return;
  S.watchQueue.push(...watch);
  if (up.length) {
    showView('transfers');
    S.pendingPaths = up;
    openDestAt(null);
  } else if ($('#dest-modal').classList.contains('hidden')) nextWatch();
}

function nextWatch() {
  const p = S.watchQueue.shift();
  if (!p) return;
  showView('watch');
  D.watchPath = p;
  D.watchDir = 'upload';
  D.watchChange = 'overwrite';
  openDest('watch');
}

function onDrop(paths) {
  if (!paths || !paths.length || !ensureCanUpload()) return;
  showView('transfers');
  startUpload(paths);
}
async function startUpload(paths) {
  if (!S.state.settings.last_folder_id) {
    S.pendingPaths = paths;
    openDest();
    return;
  }
  try {
    await call('StartUpload', paths);
  } catch (e) { toast(errMsg(e), 'err'); }
}

// ---------- downloads ----------
const setPreview = (html) => { $('#link-preview').innerHTML = html; };

async function inspectLink() {
  const link = $('#link').value.trim();
  const pass = $('#passphrase').value;
  const seq = ++S.linkSeq;
  if (!link) {
    setPreview('');
    $('#passphrase').classList.add('hidden');
    updateDownloadBtn();
    return;
  }
  setPreview('<span class="spinner"></span>');
  const li = await call('InspectLink', link, pass);
  if (seq !== S.linkSeq) return;
  S.linkInfo = li;
  if (li.needs_passphrase) {
    $('#passphrase').classList.remove('hidden');
    setPreview(`<span class="warn">${esc(li.error)}</span>`);
    if (!pass) $('#passphrase').focus();
  } else if (li.error) {
    setPreview(`<span class="err">${esc(li.error)}</span>`);
  } else {
    const unavail = li.unavailable ? `<span class="warn">· ${li.unavailable} unavailable</span>` : '';
    setPreview(`<span class="ok-dot"></span><b>${esc(li.name || 'Shared files')}</b><span class="dim">${li.files} file${li.files === 1 ? '' : 's'} · ${fmtBytes(li.total_bytes)}</span>${unavail}`);
  }
  updateDownloadBtn();
}

function updateDownloadBtn() {
  const li = S.linkInfo;
  $('#download-btn').disabled = !$('#link').value.trim() || (li && li.error && !li.needs_passphrase);
}

async function chooseDownloadDir() {
  const p = await call('PickDownloadDir', S.dlDest);
  if (p) await saveSettings({ download_dir: p });
}

async function startDownload() {
  const link = $('#link').value.trim();
  if (!link || $('#download-btn').disabled) return;
  try {
    await call('StartDownload', { link, passphrase: $('#passphrase').value, dest_dir: S.dlDest, subfolder: $('#subfolder').checked });
    $('#link').value = '';
    $('#passphrase').value = '';
    $('#passphrase').classList.add('hidden');
    S.linkInfo = null;
    S.linkSeq++;
    setPreview('');
    updateDownloadBtn();
  } catch (e) { toast(errMsg(e), 'err'); }
}

// ---------- transfers list ----------
function renderSnapshot() {
  const jobs = S.snap.jobs || [];
  let up = 0, down = 0, active = 0;
  for (const j of jobs) {
    if (!TERMINAL.has(j.status)) active++;
    if (j.kind === 'upload') up += j.speed || 0; else down += j.speed || 0;
  }
  $('#speed-up').textContent = fmtSpeed(up);
  $('#speed-down').textContent = fmtSpeed(down);
  $('#speed-up').title = up ? fmtMbit(up) : '';
  $('#speed-down').title = down ? fmtMbit(down) : '';
  $('#streams').textContent = `${S.snap.active_streams} / ${S.snap.stream_limit}`;
  $('#active-count').textContent = active;
  $('#active-count').classList.toggle('hidden', active === 0);

  const wrap = $('#jobs');
  $('#jobs-empty').classList.toggle('hidden', jobs.length > 0);
  $('#jobs-list .list-head').classList.toggle('hidden', jobs.length === 0);
  $('#clear-btn').classList.toggle('hidden', !jobs.some((j) => TERMINAL.has(j.status)));
  const seen = new Set();
  jobs.forEach((j, i) => {
    seen.add(j.id);
    let el = wrap.querySelector(`.row[data-id="${j.id}"]`);
    if (!el) el = createJob(j);
    if (wrap.children[i] !== el) wrap.insertBefore(el, wrap.children[i] || null);
    updateJob(el, j);
    const prev = S.prevStatus[j.id];
    if (prev && !TERMINAL.has(prev) && TERMINAL.has(j.status)) onJobFinished(j);
    S.prevStatus[j.id] = j.status;
  });
  [...wrap.children].forEach((el) => { if (!seen.has(el.dataset.id)) el.remove(); });
  for (const id of Object.keys(S.prevStatus)) {
    if (!seen.has(id)) { delete S.prevStatus[id]; delete S.filesFetchedAt[id]; S.expanded.delete(id); }
  }
}

function onJobFinished(j) {
  if (S.view === 'history') loadHistory();
  if (S.expanded.has(j.id)) refreshFiles(j.id, true);
  if (j.status === 'failed') toast(`${j.kind === 'upload' ? 'Upload' : 'Download'} failed: ${j.error || j.title}`, 'err', 10000);
}

function createJob(j) {
  const el = document.createElement('div');
  el.className = 'row';
  el.dataset.id = j.id;
  el.innerHTML = `
    <div class="row-main jobs-grid">
      <div class="cell-name"><div class="kind-icon">${j.kind === 'upload' ? I.up : I.down}</div><div class="names"><div class="name"></div><div class="sub"></div></div></div>
      <div class="progress"><div class="track"><i></i></div><span class="pct"></span></div>
      <div class="cell r c-size"></div>
      <div class="cell r strong c-speed"></div>
      <div class="c-status"></div>
      <div class="actions">
        <button class="icon-btn a-open" title="Open in Frame.io">${I.open}</button>
        <button class="icon-btn a-copy" title="Copy share link">${I.copy}</button>
        <button class="icon-btn a-reveal" title="Show in Finder">${I.reveal}</button>
        <button class="icon-btn a-retry" title="Retry">${I.retry}</button>
        <button class="icon-btn a-cancel" title="Cancel">${I.x}</button>
        <button class="icon-btn a-files" title="Show files">${I.chevron}</button>
      </div>
    </div>
    <div class="error-line hidden"></div>
    <div class="row-detail hidden"><div class="files"></div></div>`;
  $('.a-cancel', el).onclick = () => call('Cancel', j.id);
  $('.a-open', el).onclick = () => el.dataset.link && call('OpenURL', el.dataset.link);
  $('.a-copy', el).onclick = () => copyShare(j.id, $('.a-copy', el));
  $('.a-retry', el).onclick = () => retry(j.id);
  $('.a-reveal', el).onclick = () => call('Reveal', el.dataset.dest || '').catch((e) => toast(errMsg(e), 'err'));
  $('.a-files', el).onclick = () => toggleFiles(j.id, el);
  return el;
}

function setHTML(el, html) { if (el.dataset.h !== html) { el.innerHTML = html; el.dataset.h = html; } }

function updateJob(el, j) {
  const terminal = TERMINAL.has(j.status);
  el.className = `row ${j.kind} ${j.status}`;
  el.dataset.dest = j.dest;
  el.dataset.link = j.link || '';
  $('.name', el).textContent = j.title || (j.kind === 'upload' ? 'Upload' : 'Download');
  const sub = j.kind === 'upload' ? `To ${j.dest}` : `${shortLink(j.source)} → ${shortPath(j.dest)}`;
  $('.sub', el).textContent = j.phase && !terminal ? j.phase : sub;
  $('.sub', el).title = sub;

  const pct = j.total > 0 ? Math.min(100, (100 * j.done) / j.total) : (j.status === 'done' ? 100 : 0);
  const preparing = j.status === 'resolving' || j.status === 'queued';
  $('.track', el).classList.toggle('indet', preparing);
  $('.track i', el).style.width = `${pct}%`;
  $('.pct', el).textContent = preparing ? '' : `${Math.floor(pct)}%`;

  setHTML($('.c-size', el), j.total ? (terminal ? fmtBytes(j.total) : `${fmtBytes(j.done)} <span style="color:var(--text-4)">/</span> ${fmtBytes(j.total)}`) : '—');
  const speed = !terminal ? (j.speed > 0 ? fmtSpeed(j.speed) : '—') : (j.avg_speed > 0 ? fmtSpeed(j.avg_speed) : '—');
  $('.c-speed', el).textContent = speed;
  $('.c-speed', el).title = terminal ? 'Average speed' : (j.eta > 0 ? `${fmtDur(j.eta)} remaining` : '');
  $('.c-speed', el).style.color = terminal ? 'var(--text-2)' : '';

  let status = statusLabel(j.status, j.kind);
  if (!terminal && j.eta > 0 && !preparing) status = `${fmtDur(j.eta)} left`;
  if (!terminal && j.kind === 'upload' && j.total > 0 && j.done >= j.total) status = 'Verifying';
  if (j.status === 'partial') status = `${j.files_failed} of ${j.files_total} failed`;
  if (j.status === 'done') status = `${j.files_total} file${j.files_total === 1 ? '' : 's'} · ${fmtDur(j.elapsed)}`;
  setHTML($('.c-status', el), `<span class="status ${j.status}" title="${esc(status)}"><span class="dot"></span><span class="trunc">${esc(status)}</span></span>`);

  const err = $('.error-line', el);
  const showErr = j.status === 'failed' && j.error;
  err.classList.toggle('hidden', !showErr);
  if (showErr) { err.textContent = j.error; err.title = j.error; }

  $('.a-cancel', el).classList.toggle('hidden', terminal);
  $('.a-open', el).classList.toggle('hidden', !j.link);
  $('.a-copy', el).classList.toggle('hidden', !j.link);
  $('.a-retry', el).classList.toggle('hidden', !(terminal && j.status !== 'done'));
  $('.a-reveal', el).classList.toggle('hidden', j.kind !== 'download' || !j.dest);
  $('.a-files', el).classList.toggle('hidden', j.files_total === 0);
  setHTML($('.a-files', el), S.expanded.has(j.id) ? I.chevronUp : I.chevron);
  if (S.expanded.has(j.id) && !terminal) refreshFiles(j.id);
}

function toggleFiles(id, el) {
  const open = !S.expanded.has(id);
  if (open) S.expanded.add(id); else S.expanded.delete(id);
  $('.row-detail', el).classList.toggle('hidden', !open);
  if (open) refreshFiles(id, true);
  const j = (S.snap.jobs || []).find((x) => x.id === id);
  if (j) updateJob(el, j);
}

function fileRows(files, kind) {
  const order = { running: 0, failed: 1, queued: 2, canceled: 3, done: 4, skipped: 4 };
  const sorted = files.map((f, i) => ({ ...f, i })).sort((a, b) => (order[a.status] ?? 5) - (order[b.status] ?? 5) || a.i - b.i);
  const shown = sorted.slice(0, 400);
  const html = shown.map((f) => {
    const done = f.done ?? (f.status === 'done' ? f.size : 0);
    const pct = f.size > 0 ? Math.min(100, (100 * done) / f.size) : (f.status === 'done' ? 100 : 0);
    let st = statusLabel(f.status, kind);
    if (f.status === 'failed') st = f.error || 'Failed';
    else if (f.status === 'skipped') st = f.note || 'Skipped';
    else if ((f.status === 'done' || f.status === 'running') && f.note) st = f.note;
    if (f.retries > 0 && !TERMINAL.has(f.status)) st += ` · ${f.retries} retr${f.retries === 1 ? 'y' : 'ies'}`;
    const open = f.view_url ? `<a data-url="${esc(f.view_url)}" title="Open in Frame.io">${I.external}</a>` : '';
    const reveal = f.local_path && TERMINAL.has(f.status) && f.status !== 'failed' ? `<a class="reveal" data-path="${esc(f.local_path)}" title="Show in Finder">${I.external}</a>` : '';
    const bar = f.status === 'failed' ? 'background:var(--error)' : (f.status === 'done' || f.status === 'skipped') ? 'background:var(--text-4)' : '';
    return `<div class="frow"><div class="fn" title="${esc(f.rel_path)}"><span>${esc(f.rel_path)}</span>${open || reveal}</div>
      <div class="fs">${fmtBytes(f.size)}</div>
      <div class="track"><i style="width:${pct}%;${bar}"></i></div>
      <div class="fst ${f.status}" title="${esc(st)}">${esc(st)}</div></div>`;
  }).join('');
  const more = files.length > shown.length ? `<div class="files-more">+ ${files.length - shown.length} more files</div>` : '';
  return html + more;
}

function bindReveal(root) {
  $$('a.reveal', root).forEach((a) => a.addEventListener('click', (e) => {
    e.preventDefault();
    call('Reveal', a.dataset.path).catch((err) => toast(errMsg(err), 'err'));
  }));
}

async function refreshFiles(id, force) {
  const now = Date.now();
  if (!force && now - (S.filesFetchedAt[id] || 0) < 900) return;
  S.filesFetchedAt[id] = now;
  const files = (await call('JobFiles', id)) || [];
  const box = $(`.row[data-id="${id}"] .files`);
  if (!box) return;
  const kind = ((S.snap.jobs || []).find((x) => x.id === id) || {}).kind || '';
  box.innerHTML = fileRows(files, kind);
  bindReveal(box);
}

async function retry(id) {
  try {
    const newId = await call('Retry', id, '');
    if (!newId) { toast('Its watch folder will retry these files', 'ok'); loadWatches(); return; }
    showView('transfers');
  } catch (e) { toast(errMsg(e), 'err'); }
}

// ---------- destination picker ----------
// items: the current level; q: the search text, matched against items here and
// (hits) against the whole account; rows: what the list shows, for the keyboard.
const D = { stack: [], mode: 'upload', watchPath: '', watchDir: 'upload', watchChange: 'overwrite', items: null, err: '', q: '', hits: null, hitsErr: '', rows: [], sel: 0, kbd: false, seq: 0, qseq: 0 };
const DIR_ARROW = { upload: '→', download: '←', mirror: '↔' };

// start: the path to open at, or a function returning a promise of one.
function openDest(mode = 'upload', onPick = null, start = null) {
  D.mode = mode;
  D.onPick = onPick;
  $('#dest-opts').classList.toggle('hidden', mode !== 'watch');
  $('#dest-dir').classList.toggle('hidden', mode !== 'watch');
  $('#dest-existing').checked = false;
  renderDestTitle();
  $('#dest-choose').textContent = mode === 'watch' ? 'Watch' : mode === 'pick' ? 'Choose' : 'Upload here';
  $('#dest-modal').classList.remove('hidden');
  D.stack = [];
  $('#newfolder-name').classList.add('hidden');
  clearQuery();
  $('#dest-q').focus();
  if (!start) return loadLevel(true);
  const seq = ++D.seq;
  D.items = null;
  D.err = '';
  renderCrumbs();
  renderList();
  $('#dest-choose').disabled = $('#newfolder-btn').disabled = true;
  Promise.resolve(typeof start === 'function' ? start() : start).then((path) => {
    if (seq !== D.seq) return;
    D.stack = path ? path.slice() : [];
    loadLevel(!path);
  });
}

// openDestAt opens the picker at a level of the upload destination: -1 is the
// project list, 0 the project, 1… its folders, null the destination itself.
function openDestAt(k) {
  if (!S.state.settings.last_folder_id) return openDest();
  openDest('upload', null, async () => {
    const path = await destPath();
    if (!path || k === null) return path;
    const pi = path.findIndex((x) => x.kind === 'project');
    return path.slice(0, Math.min(path.length, pi + 1 + k));
  });
}

function storedPath(s) {
  const p = s.last_folder_path;
  return p && p.length && p[p.length - 1].folder_id === s.last_folder_id ? p : null;
}

// destPath returns the path down to the upload destination, asking Frame.io
// for it once if it was chosen before paths were saved.
async function destPath() {
  const s = S.state.settings;
  const stored = storedPath(s);
  if (stored) return stored;
  try {
    const path = await call('PathTo', s.last_account_id, 'folder', s.last_folder_id);
    S.state = await call('SetUploadDestination', s.last_account_id, s.last_folder_id, s.last_folder_label, path);
    renderState();
    return path;
  } catch (e) { return null; }
}
function renderDestTitle() {
  $$('#dest-dir button').forEach((b) => b.classList.toggle('active', b.dataset.dir === D.watchDir));
  $$('#dest-change button').forEach((b) => b.classList.toggle('active', b.dataset.change === D.watchChange));
  $('#dest-change').classList.toggle('hidden', !ups(D.watchDir));
  $('#dest-change-label').classList.toggle('hidden', !ups(D.watchDir));
  const n = S.pendingPaths ? S.pendingPaths.length : 0;
  $('#dest-modal .modal-title').textContent = D.mode === 'watch' ? `${baseName(D.watchPath)} ${DIR_ARROW[D.watchDir]} Frame.io`
    : D.mode === 'pick' ? 'Frame.io folder' : n === 1 ? `Upload ${baseName(S.pendingPaths[0])} to` : n ? `Upload ${n} items to` : 'Upload to';
}
function closeDest() {
  $('#dest-modal').classList.add('hidden');
  if (D.mode !== 'pick') S.pendingPaths = null;
  if (D.mode === 'watch') S.watchQueue = [];
}
const destTop = () => D.stack[D.stack.length - 1];

const inFolderLevel = () => { const t = destTop(); return !!(t && (t.kind === 'project' || t.kind === 'folder')); };

async function loadLevel(autoSkip) {
  const seq = ++D.seq;
  renderCrumbs();
  const t = destTop();
  $('#dest-choose').disabled = !inFolderLevel();
  $('#newfolder-btn').disabled = !inFolderLevel();
  D.items = null;
  D.err = '';
  renderList();
  let items;
  try {
    if (!t) items = await call('ListAccounts');
    else if (t.kind === 'account') items = await call('ListWorkspaces', t.id);
    else if (t.kind === 'workspace') items = await call('ListProjects', D.stack[0].id, t.id);
    else items = await call('ListFolders', D.stack[0].id, t.folder_id);
  } catch (e) {
    if (seq !== D.seq) return;
    S.state = await call('GetState');
    renderState();
    if (!S.state.logged_in) { closeDest(); toast('Signed out — sign in again', 'err'); return; }
    D.items = [];
    D.err = errMsg(e);
    renderList();
    return;
  }
  if (seq !== D.seq) return;
  items = items || [];
  if (autoSkip && items.length === 1 && (!t || t.kind === 'account')) {
    D.stack.push(items[0]);
    return loadLevel(true);
  }
  D.items = items;
  renderList();
}

// hl marks the part of a name that matches the search.
function hl(name) {
  const q = D.q.trim();
  const i = q ? name.toLowerCase().indexOf(q.toLowerCase()) : -1;
  if (i < 0) return esc(name);
  return `${esc(name.slice(0, i))}<mark>${esc(name.slice(i, i + q.length))}</mark>${esc(name.slice(i + q.length))}`;
}

function renderList() {
  const list = $('#dest-list');
  if (D.err) { list.innerHTML = `<div class="dest-msg" style="color:var(--error-text)">${esc(D.err)}</div>`; return; }
  if (!D.items) { list.innerHTML = '<div class="dest-msg"><span class="spinner"></span></div>'; return; }
  const q = D.q.trim().toLowerCase();
  const here = D.items.filter((it) => !q || it.name.toLowerCase().includes(q));
  const ids = new Set(here.map((it) => it.id));
  const there = q && D.hits ? D.hits.filter((h) => !ids.has(h.id)) : [];
  const everywhere = q.length >= 2 && !!searchAcct();
  D.rows = [...here, ...there];
  D.sel = Math.max(0, Math.min(D.sel, D.rows.length - 1));
  const mark = q || D.kbd;
  const ico = { account: I.team, workspace: I.team, project: I.project, folder: I.folder };
  const row = (it, i) => `<div class="dest-item${mark && i === D.sel ? ' sel' : ''}" data-i="${i}">${ico[it.kind]}<span class="nm">${hl(it.name)}</span>${it.where ? `<span class="where">${esc(it.where)}</span>` : ''}<span class="chev">›</span></div>`;
  let html = here.map(row).join('');
  if (!q && !here.length) html = `<div class="dest-msg">${inFolderLevel() ? 'No folders' : 'Empty'}</div>`;
  else if (everywhere && (D.hitsErr || !D.hits || there.length)) {
    html += '<div class="dest-sec">All projects</div>';
    if (D.hitsErr) html += `<div class="dest-note err">${esc(D.hitsErr)}</div>`;
    else if (!D.hits) html += '<div class="dest-note"><span class="spinner"></span></div>';
    else html += there.map((it, k) => row(it, here.length + k)).join('');
  } else if (!here.length) html = '<div class="dest-msg">No matches</div>';
  list.innerHTML = html;
  $$('.dest-item', list).forEach((el) => el.addEventListener('click', () => openRow(D.rows[+el.dataset.i])));
  const sel = $('.dest-item.sel', list);
  if (sel) sel.scrollIntoView({ block: 'nearest' });
}

// openRow goes into a row: a level below this one, or a search result anywhere.
async function openRow(it) {
  if (!it) return;
  clearQuery();
  if (!it.remote) {
    D.stack.push(it);
    return loadLevel(true);
  }
  const seq = ++D.seq;
  D.items = null;
  renderList();
  try {
    const path = await call('PathTo', searchAcct(), it.kind, it.id);
    if (seq !== D.seq) return;
    D.stack = path;
  } catch (e) {
    if (seq !== D.seq) return;
    toast(errMsg(e), 'err');
  }
  loadLevel(false);
}

function searchAcct() {
  const a = D.stack.find((x) => x.kind === 'account');
  return a ? a.id : S.state.settings.last_account_id || '';
}

function clearQuery() {
  Object.assign(D, { q: '', hits: null, hitsErr: '', sel: 0, kbd: false });
  D.qseq++;
  $('#dest-q').value = '';
}

const searchSoon = debounce(searchEverywhere, 300);
function onDestQuery() {
  Object.assign(D, { q: $('#dest-q').value, hits: null, hitsErr: '', sel: 0 });
  D.qseq++;
  renderList();
  if (D.q.trim().length >= 2 && searchAcct()) searchSoon();
}

async function searchEverywhere() {
  const q = D.q.trim(), acct = searchAcct();
  if (q.length < 2 || !acct) return;
  const seq = ++D.qseq;
  try {
    const hits = (await call('Search', acct, q)) || [];
    if (seq !== D.qseq) return;
    D.hits = hits.map((h) => ({ ...h, remote: true }));
  } catch (e) {
    if (seq !== D.qseq) return;
    D.hitsErr = errMsg(e);
  }
  renderList();
}

function onDestKey(e) {
  if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
    e.preventDefault();
    if (!D.rows.length) return;
    if (D.q || D.kbd) D.sel = Math.max(0, Math.min(D.rows.length - 1, D.sel + (e.key === 'ArrowDown' ? 1 : -1)));
    D.kbd = true;
    renderList();
  } else if (e.key === 'Enter' && (D.q || D.kbd)) {
    e.preventDefault();
    openRow(D.rows[D.sel]);
  } else if (e.key === 'Escape' && D.q) {
    e.stopPropagation();
    clearQuery();
    renderList();
  }
}

function renderCrumbs() {
  const c = $('#dest-crumbs');
  const parts = [{ name: 'Frame.io' }, ...D.stack];
  c.innerHTML = parts.map((p, i) => `${i ? '<span>/</span>' : ''}<button data-i="${i}" class="${i === parts.length - 1 ? 'cur' : ''}">${esc(p.name)}</button>`).join('');
  $$('button', c).forEach((b) => b.addEventListener('click', () => {
    clearQuery();
    D.stack = D.stack.slice(0, +b.dataset.i);
    loadLevel(false);
  }));
}

function destLabel() {
  const pi = D.stack.findIndex((x) => x.kind === 'project');
  return D.stack.slice(pi).map((x) => x.name).join(' / ');
}

async function chooseDest() {
  const t = destTop();
  if (!t) return;
  if (D.mode === 'pick') {
    $('#dest-modal').classList.add('hidden');
    if (D.onPick) D.onPick({ acct: D.stack[0].id, folder: t.folder_id, label: destLabel() });
    return;
  }
  if (D.mode === 'watch') {
    try {
      await call('AddWatch', D.watchPath, D.stack[0].id, t.folder_id, destLabel(), D.watchDir, D.watchChange, $('#dest-existing').checked);
      $('#dest-modal').classList.add('hidden');
      showView('watch');
      nextWatch();
    } catch (e) { toast(errMsg(e), 'err'); }
    return;
  }
  try {
    const path = D.stack.map(({ id, name, kind, folder_id }) => ({ id, name, kind, folder_id }));
    S.state = await call('SetUploadDestination', D.stack[0].id, t.folder_id, destLabel(), path);
    renderState();
    const pending = S.pendingPaths;
    $('#dest-modal').classList.add('hidden');
    S.pendingPaths = null;
    if (pending) startUpload(pending);
  } catch (e) { toast(errMsg(e), 'err'); }
}

async function newFolder() {
  const input = $('#newfolder-name');
  if (input.classList.contains('hidden')) {
    input.classList.remove('hidden');
    input.value = '';
    input.focus();
    return;
  }
  const name = input.value.trim();
  if (!name) return input.focus();
  try {
    const f = await call('CreateFolder', D.stack[0].id, destTop().folder_id, name);
    input.classList.add('hidden');
    clearQuery();
    D.stack.push(f);
    loadLevel(false);
  } catch (e) { toast(errMsg(e), 'err'); }
}

// ---------- history ----------
async function loadHistory() {
  let rows;
  try { rows = (await call('History', $('#history-q').value, S.histKind, 500)) || []; } catch (e) { return toast(errMsg(e), 'err'); }
  const active = new Set((S.snap.jobs || []).filter((j) => !TERMINAL.has(j.status)).map((j) => j.id));
  $('#history-empty').classList.toggle('hidden', rows.length > 0);
  $('#view-history .list-head').classList.toggle('hidden', rows.length === 0);
  const body = $('#history-body');
  body.innerHTML = rows.map((t) => {
    const status = active.has(t.id) ? 'running' : t.status;
    const dur = t.finished_at && t.started_at ? (t.finished_at - t.started_at) / 1000 : 0;
    const sub = t.kind === 'upload' ? `To ${t.dest}` : `${shortLink(t.source)} → ${shortPath(t.dest)}`;
    const canRetry = !active.has(t.id) && ['failed', 'partial', 'canceled', 'interrupted'].includes(t.status);
    const open = S.histExpanded.has(t.id);
    return `<div class="row ${status}" data-id="${t.id}">
      <div class="row-main hist-grid clickable-row">
        <div class="cell-name"><div class="kind-icon">${t.kind === 'upload' ? I.up : I.down}</div><div class="names"><div class="name" title="${esc(t.title)}">${esc(t.title)}</div><div class="sub" title="${esc(sub)}">${esc(sub)}</div></div></div>
        <div class="cell" title="${esc(new Date(t.created_at).toLocaleString())}">${fmtDate(t.created_at)}</div>
        <div class="cell r h-files" title="${t.files_done} of ${t.files_total} files">${t.files_done}/${t.files_total}</div>
        <div class="cell r h-size">${fmtBytes(t.total_bytes)}</div>
        <div class="cell r h-speed" title="${t.avg_bps ? fmtMbit(t.avg_bps) : ''}">${fmtSpeed(t.avg_bps)}</div>
        <div class="cell r h-dur">${fmtDur(dur)}</div>
        <div>${statusHTML(status, t.kind)}</div>
        <div class="actions">
          ${t.link ? `<button class="icon-btn h-open" title="Open in Frame.io">${I.open}</button>` : ''}
          ${canRetry ? `<button class="icon-btn h-retry" title="Retry">${I.retry}</button>` : ''}
          ${t.kind === 'download' ? `<button class="icon-btn h-reveal" title="Show in Finder">${I.reveal}</button>` : ''}
        </div>
      </div>
      ${open ? '<div class="row-detail"><div class="hist-detail"></div></div>' : ''}
    </div>`;
  }).join('');
  const byId = Object.fromEntries(rows.map((r) => [r.id, r]));
  $$('.row', body).forEach((row) => {
    const t = byId[row.dataset.id];
    $('.row-main', row).addEventListener('click', (e) => {
      if (e.target.closest('button')) return;
      if (S.histExpanded.has(t.id)) S.histExpanded.delete(t.id); else S.histExpanded.add(t.id);
      loadHistory();
    });
    const r = $('.h-retry', row); if (r) r.onclick = () => retry(t.id);
    const o = $('.h-open', row); if (o) o.onclick = () => call('OpenURL', t.link);
    const v = $('.h-reveal', row); if (v) v.onclick = () => call('Reveal', t.dest).catch((e) => toast(errMsg(e), 'err'));
    if (S.histExpanded.has(t.id)) renderHistoryDetails(t, $('.hist-detail', row));
  });
}

async function renderHistoryDetails(t, box) {
  const files = (await call('HistoryFiles', t.id)) || [];
  const meta = [
    [t.kind === 'upload' ? 'From' : 'Link', t.source],
    [t.kind === 'upload' ? 'To' : 'Saved to', t.dest],
    ['Started', t.started_at ? new Date(t.started_at).toLocaleString() : '—'],
    ['Finished', t.finished_at ? new Date(t.finished_at).toLocaleString() : '—'],
    ['Transferred', `${fmtBytes(t.done_bytes)} of ${fmtBytes(t.total_bytes)}`],
    ['Speed', t.avg_bps ? `${fmtSpeed(t.avg_bps)} average (${fmtMbit(t.avg_bps)})${t.peak_bps ? ` · ${fmtSpeed(t.peak_bps)} peak` : ''}` : '—'],
    ['Transfer ID', t.id],
  ];
  if (t.link) meta.splice(2, 0, ['Frame.io', t.link]);
  if (t.share_link) meta.splice(t.link ? 3 : 2, 0, ['Share', t.share_link]);
  if (t.error) meta.push(['Error', t.error]);
  box.innerHTML = `<div class="meta">${meta.map(([k, v]) => `<span class="k">${esc(k)}</span>${k === 'Frame.io' ? `<span class="v"><a data-url="${esc(v)}">${esc(v)}</a> · <a class="copy-share">Copy share link</a></span>` : k === 'Share' ? `<span class="v"><a data-url="${esc(v)}">${esc(v)}</a></span>` : `<span class="v selectable">${esc(v)}</span>`}`).join('')}</div>
    <div class="files">${files.length ? fileRows(files, t.kind) : '<div class="files-more">No files</div>'}</div>`;
  bindReveal(box);
  $$('a.copy-share', box).forEach((a) => a.addEventListener('click', (e) => { e.preventDefault(); copyShare(t.id); }));
}

// ---------- watch folders ----------
const W = { confirm: null };

async function addWatch() {
  if (!ensureCanUpload()) return;
  const p = await call('PickWatchFolder');
  if (!p) return;
  D.watchPath = p;
  D.watchDir = 'upload';
  D.watchChange = 'overwrite';
  openDest('watch');
}

const WATCH_STATUS = { watching: 'Watching', paused: 'Paused', uploading: 'Uploading', downloading: 'Downloading', settling: 'Copying', waiting: 'Waiting', error: 'Error' };
const DIR_ICON = { upload: I.up, download: I.down, mirror: I.sync };
const DIR_VERB = { upload: 'Uploads to', download: 'Downloads from', mirror: 'Mirrors' };
const ups = (d) => d !== 'download';
const downs = (d) => d !== 'upload';

async function loadWatches() {
  let list;
  try { list = (await call('Watches')) || []; } catch { return; }
  const active = list.filter((w) => w.state === 'uploading').length;
  $('#watch-count').textContent = list.filter((w) => w.enabled).length;
  $('#watch-count').classList.toggle('hidden', !list.some((w) => w.enabled));
  if (S.view !== 'watch') return;
  $('#watch-empty').classList.toggle('hidden', list.length > 0);
  $('#view-watch .list-head').classList.toggle('hidden', list.length === 0);
  const pause = '<svg viewBox="0 0 20 20"><path d="M7.5 5v10M12.5 5v10" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"/></svg>';
  const play = '<svg viewBox="0 0 20 20"><path d="M7 5.5v9l7-4.5z" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linejoin="round"/></svg>';
  const trash = '<svg viewBox="0 0 20 20"><path d="M4.5 6h11M8 6V4.5h4V6M6 6l.7 9.5h6.6L14 6" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"/></svg>';
  $('#watch-body').innerHTML = list.map((w) => {
    const label = WATCH_STATUS[w.state] + (w.detail ? ` · ${w.detail}` : '');
    const tip = w.state === 'waiting' ? `${w.detail} not ready on Frame.io yet` : label;
    const conf = W.confirm === w.id;
    const dir = w.direction || 'upload';
    const count = dir === 'mirror' ? `↑ ${w.uploaded}  ↓ ${w.downloaded}` : String(dir === 'download' ? w.downloaded : w.uploaded);
    const countTip = [ups(dir) && `${w.uploaded} uploaded`, downs(dir) && `${w.downloaded} downloaded`, w.failed && `${w.failed} failed`, w.canceled && `${w.canceled} canceled`].filter(Boolean).join(' · ');
    return `<div class="row ${w.enabled ? '' : 'paused'}" data-id="${w.id}">
      <div class="row-main watch-grid">
        <div class="cell-name"><div class="kind-icon">${I.folder}</div><div class="names"><div class="name">${esc(w.name)}</div><div class="sub" title="${esc(w.local_path)}">${esc(shortPath(w.local_path))}</div></div></div>
        <div class="cell dir-cell" title="${esc(`${DIR_VERB[dir]} ${w.dest_label}`)}">${DIR_ICON[dir]}<span class="trunc">${esc(w.dest_label)}</span></div>
        <div><span class="status ${w.state}" title="${esc(tip)}"><span class="dot"></span><span class="trunc">${esc(label)}</span></span></div>
        <div class="cell r w-uploaded" title="${esc(countTip)}">${esc(count)}${w.failed ? ` <span style="color:var(--error-text)">· ${w.failed} failed</span>` : ''}${w.canceled ? ` <span class="muted-note">· ${w.canceled} canceled</span>` : ''}</div>
        <div class="cell w-last">${w.last_transfer ? fmtDate(w.last_transfer) : '—'}</div>
        <div class="actions">
          <button class="icon-btn w-toggle" title="${w.enabled ? 'Pause' : 'Resume'}">${w.enabled ? pause : play}</button>
          <button class="icon-btn w-edit" title="Edit">${I.pencil}</button>
          ${w.link ? `<button class="icon-btn w-open" title="Open in Frame.io">${I.open}</button>` : ''}
          <button class="icon-btn w-reveal" title="Show in Finder">${I.reveal}</button>
          <button class="icon-btn w-remove ${conf ? 'confirm' : ''}" title="Remove">${conf ? 'Remove' : trash}</button>
        </div>
      </div>
    </div>`;
  }).join('');
  const byId = Object.fromEntries(list.map((w) => [w.id, w]));
  $$('#watch-body .row').forEach((row) => {
    const w = byId[row.dataset.id];
    $('.w-toggle', row).onclick = async () => { await call('SetWatchEnabled', w.id, !w.enabled); loadWatches(); };
    $('.w-reveal', row).onclick = () => call('Reveal', w.local_path).catch((e) => toast(errMsg(e), 'err'));
    $('.w-edit', row).onclick = () => openWatchEdit(w);
    const wo = $('.w-open', row); if (wo) wo.onclick = () => call('OpenURL', w.link);
    $('.w-remove', row).onclick = async () => {
      if (W.confirm !== w.id) { W.confirm = w.id; loadWatches(); setTimeout(() => { if (W.confirm === w.id) { W.confirm = null; loadWatches(); } }, 3000); return; }
      W.confirm = null;
      await call('RemoveWatch', w.id);
      loadWatches();
    };
  });
}

// ---------- watch folder editor ----------
const WE = {};

function openWatchEdit(w) {
  const dir = w.direction || 'upload';
  Object.assign(WE, { id: w.id, local: w.local_path, acct: w.account_id, folder: w.folder_id, label: w.dest_label, dir, change: w.on_change || 'overwrite', canceled: w.canceled,
    orig: { local: w.local_path, acct: w.account_id, folder: w.folder_id, dir } });
  $('#we-existing').checked = false;
  renderWatchEdit();
  $('#watch-modal').classList.remove('hidden');
}

function renderWatchEdit() {
  $('#we-local').textContent = shortPath(WE.local);
  $('#we-local').title = WE.local;
  $('#we-dest').textContent = WE.label;
  $('#we-dest').title = WE.label;
  $$('#we-dir button').forEach((b) => b.classList.toggle('active', b.dataset.dir === WE.dir));
  $$('#we-change button').forEach((b) => b.classList.toggle('active', b.dataset.change === WE.change));
  $('#we-change-row').classList.toggle('hidden', !ups(WE.dir));
  $('#we-existing-row').classList.toggle('hidden', !freshSide());
  $('#we-canceled-row').classList.toggle('hidden', !WE.canceled);
  $('#we-requeue').textContent = { upload: 'Upload them', download: 'Download them', mirror: 'Transfer them' }[WE.orig.dir];
  $('#we-canceled').textContent = `${WE.canceled} file${WE.canceled === 1 ? '' : 's'}`;
}

// Same rule as Watcher.Update: a side the watch takes files from starts over
// when it's new or points elsewhere (for a mirror, when either end changes).
function freshSide() {
  const o = WE.orig, m = WE.dir === 'mirror';
  const lc = WE.local !== o.local, dc = WE.folder !== o.folder || WE.acct !== o.acct;
  return (ups(WE.dir) && (lc || (m && dc) || !ups(o.dir))) || (downs(WE.dir) && (dc || (m && lc) || !downs(o.dir)));
}

function closeWatchEdit() { $('#watch-modal').classList.add('hidden'); }

async function saveWatchEdit() {
  try {
    await call('UpdateWatch', WE.id, WE.local, WE.acct, WE.folder, WE.label, WE.dir, WE.change, $('#we-existing').checked);
    closeWatchEdit();
    loadWatches();
  } catch (e) { toast(errMsg(e), 'err'); }
}
