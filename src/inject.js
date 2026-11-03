/* pi-history: injected at serve time. Every feature is isolated in try/catch; if the export's structure
   or data shape is not what we expect, nothing is changed. */
(function () {
  'use strict';
  function safe(fn) { try { fn(); } catch (e) { try { console.warn('[pi-history]', e); } catch (_) {} } }

  var data = null;
  safe(function () {
    var el = document.getElementById('session-data');
    if (!el) return;
    var bin = atob(el.textContent.trim());
    var bytes = new Uint8Array(bin.length);
    for (var i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
    var d = JSON.parse(new TextDecoder('utf-8').decode(bytes));
    if (d && Array.isArray(d.entries)) data = d;
  });

  function fmtTok(n) {
    n = n || 0;
    if (n < 1000) return String(n);
    if (n < 1e6) return (n / 1000).toFixed(n < 1e4 ? 1 : 0).replace(/\.0$/, '') + 'k';
    return (n / 1e6).toFixed(1).replace(/\.0$/, '') + 'M';
  }
  function costOf(u) {
    var c = u && u.cost;
    if (!c) return null;
    if (typeof c.total === 'number') return c.total;
    return (c.input || 0) + (c.output || 0) + (c.cacheRead || 0) + (c.cacheWrite || 0);
  }
  function fmtCost(v) { return '$' + (v < 0.01 ? v.toFixed(4) : v.toFixed(v < 10 ? 3 : 2)); }
  function fmtDur(ms) {
    if (!(ms > 0)) return '';
    var m = Math.round(ms / 60000);
    if (m < 1) return Math.max(1, Math.round(ms / 1000)) + '秒';
    if (m < 60) return m + '分';
    var h = Math.floor(m / 60);
    if (h < 24) return h + '時間' + (m % 60 ? (m % 60) + '分' : '');
    return Math.floor(h / 24) + '日' + (h % 24) + '時間';
  }
  var ESC = { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' };
  function esc(s) { return String(s).replace(/[&<>"']/g, function (c) { return ESC[c]; }); }
  function pad(n) { return n < 10 ? '0' + n : '' + n; }
  function fmtDate(t) {
    var d = new Date(t);
    if (isNaN(d)) return '';
    return d.getFullYear() + '-' + pad(d.getMonth() + 1) + '-' + pad(d.getDate()) + ' ' + pad(d.getHours()) + ':' + pad(d.getMinutes());
  }

  // ---- "Aa" font settings (CSS variables --ph-*; helpers live in the <head> script as window.piHistoryFont) ----
  var FONT_PRESETS = [
    ['', '既定（システムUI）'],
    ['"Yu Gothic UI", "Meiryo UI", sans-serif', 'Yu Gothic UI'],
    ['"Meiryo UI", Meiryo, sans-serif', 'Meiryo UI'],
    ['"BIZ UDPGothic", sans-serif', 'BIZ UDPGothic'],
    ['"BIZ UDGothic", monospace', 'BIZ UDGothic'],
    ['"Noto Sans JP", "Noto Sans CJK JP", sans-serif', 'Noto Sans JP'],
    ['"Segoe UI", "Yu Gothic UI", sans-serif', 'Segoe UI'],
    ['"Yu Mincho", YuMincho, "Hiragino Mincho ProN", serif', 'serif（游明朝）'],
    ['ui-monospace, "Cascadia Code", Consolas, "BIZ UDGothic", monospace', 'monospace']
  ];
  var MONO_PRESETS = [
    ['', '既定'],
    ['"Cascadia Mono", "Cascadia Code", Consolas, monospace', 'Cascadia Mono'],
    ['Consolas, "BIZ UDGothic", monospace', 'Consolas'],
    ['"BIZ UDGothic", monospace', 'BIZ UDGothic'],
    ['"MS Gothic", monospace', 'MS Gothic'],
    ['"Source Code Pro", Consolas, monospace', 'Source Code Pro'],
    ['monospace', 'monospace']
  ];
  var DEF = { size: 14.5, line: 1.75, monoSize: 13 };

  function buildFontUI(bar) {
    var F = window.piHistoryFont;
    if (!F || !bar) return;
    var btn = bar.querySelector('.pi-fontbtn');
    if (!btn) return;
    var serverEl = document.getElementById('pi-prefs-json');
    var state = {}, serverHad = false;
    try { if (serverEl) { state = F.clean(JSON.parse(serverEl.textContent)); serverHad = true; } } catch (e) { state = {}; }
    if (!serverHad) state = F.readLocal();

    var pop = document.createElement('div');
    pop.id = 'pi-font-pop';
    pop.setAttribute('role', 'dialog');
    pop.setAttribute('aria-label', '本文フォントの設定');
    pop.hidden = true;
    var ctl = {}, outs = {};
    function h3(t) { var h = document.createElement('h3'); h.textContent = t; pop.appendChild(h); }
    function row(label, id, nodes) {
      var l = document.createElement('label');
      l.textContent = label;
      if (id) l.setAttribute('for', id);
      var c = document.createElement('div');
      c.className = 'pi-ctl';
      nodes.forEach(function (n) { c.appendChild(n); });
      pop.appendChild(l);
      pop.appendChild(c);
    }
    function select(id, list) {
      var s = document.createElement('select');
      s.id = id;
      list.forEach(function (p) { var o = document.createElement('option'); o.value = p[0]; o.textContent = p[1]; s.appendChild(o); });
      return s;
    }
    function text(id, ph) {
      var t = document.createElement('input');
      t.type = 'text'; t.id = id; t.placeholder = ph; t.spellcheck = false; t.autocomplete = 'off'; t.maxLength = 200;
      t.setAttribute('aria-label', '自由入力 font-family（入力があれば優先）');
      return t;
    }
    function range(id, key, min, max, step) {
      var r = document.createElement('input');
      r.type = 'range'; r.id = id; r.min = min; r.max = max; r.step = step;
      var o = document.createElement('output');
      o.setAttribute('for', id);
      outs[key] = o;
      var w = document.createElement('div');
      w.className = 'pi-row';
      w.appendChild(r); w.appendChild(o);
      ctl[key] = r;
      return w;
    }
    h3('本文');
    ctl.fontPreset = select('pi-f-preset', FONT_PRESETS);
    ctl.font = text('pi-f-font', '自由入力（あれば優先）例: "BIZ UDPGothic"');
    row('フォント', 'pi-f-preset', [ctl.fontPreset, ctl.font]);
    row('サイズ', 'pi-f-size', [range('pi-f-size', 'size', 12, 24, 0.5)]);
    row('行間', 'pi-f-line', [range('pi-f-line', 'line', 1.3, 2.2, 0.05)]);
    h3('コード・ツール出力（等幅）');
    ctl.monoPreset = select('pi-f-mpreset', MONO_PRESETS);
    ctl.mono = text('pi-f-mono', '自由入力（あれば優先）例: "Cascadia Mono"');
    row('フォント', 'pi-f-mpreset', [ctl.monoPreset, ctl.mono]);
    row('サイズ', 'pi-f-msize', [range('pi-f-msize', 'monoSize', 10, 24, 0.5)]);
    var foot = document.createElement('div');
    foot.className = 'pi-foot';
    var note = document.createElement('span');
    note.className = 'pi-note';
    note.setAttribute('role', 'status');
    note.setAttribute('aria-live', 'polite');
    var reset = document.createElement('button');
    reset.type = 'button'; reset.textContent = 'リセット';
    foot.appendChild(note); foot.appendChild(reset);
    pop.appendChild(foot);
    bar.appendChild(pop);

    function fmt(key, v) { return key === 'line' ? String(v) : v + 'px'; }
    function show(p) {
      ctl.fontPreset.value = p.fontPreset || '';
      if (ctl.fontPreset.value !== (p.fontPreset || '')) ctl.fontPreset.value = '';
      ctl.monoPreset.value = p.monoPreset || '';
      if (ctl.monoPreset.value !== (p.monoPreset || '')) ctl.monoPreset.value = '';
      ctl.font.value = p.font || '';
      ctl.mono.value = p.mono || '';
      ['size', 'line', 'monoSize'].forEach(function (k) {
        var v = p[k] !== undefined ? p[k] : DEF[k];
        ctl[k].value = v;
        outs[k].textContent = fmt(k, v);
      });
      ctl.font.removeAttribute('aria-invalid'); ctl.mono.removeAttribute('aria-invalid');
    }
    function read() {
      var raw = {};
      if (ctl.fontPreset.value) raw.fontPreset = ctl.fontPreset.value;
      if (ctl.monoPreset.value) raw.monoPreset = ctl.monoPreset.value;
      var ft = ctl.font.value.trim(), mt = ctl.mono.value.trim();
      if (ft) raw.font = ft;
      if (mt) raw.mono = mt;
      ['size', 'line', 'monoSize'].forEach(function (k) {
        var v = parseFloat(ctl[k].value);
        outs[k].textContent = isFinite(v) ? fmt(k, v) : '';
        if (isFinite(v) && Math.abs(v - DEF[k]) > 1e-9) raw[k] = v;
      });
      var p = F.clean(raw);
      var bad = false;
      [['font', ft], ['mono', mt]].forEach(function (x) {
        var invalid = !!x[1] && p[x[0]] === undefined;
        if (invalid) { ctl[x[0]].setAttribute('aria-invalid', 'true'); bad = true; } else ctl[x[0]].removeAttribute('aria-invalid');
      });
      return { p: p, bad: bad };
    }

    var timer = null, seq = 0;
    function say(t) { note.textContent = t; }
    function persist() {
      var mySeq = ++seq, p = state, empty = Object.keys(p).length === 0;
      try { if (empty) localStorage.removeItem(F.KEY); else localStorage.setItem(F.KEY, JSON.stringify(p)); } catch (e) {}
      var body = empty ? { reset: true } : p;
      fetch('/api/ui-prefs', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) })
        .then(function (r) { return r.json().then(function (j) { return !!(r.ok && j && j.ok); }); })
        .catch(function () { return false; })
        .then(function (ok) { if (mySeq === seq) say(ok ? '保存しました（全セッション共通）' : 'このブラウザにのみ保存しました'); });
    }
    function changed() {
      safe(function () {
        var r = read();
        say(r.bad ? '使えない文字があります（; { } < > ( ) など）' : '');
        state = r.p;
        F.apply(state);
        clearTimeout(timer);
        timer = setTimeout(function () { safe(persist); }, 350);
      });
    }
    show(state);
    pop.addEventListener('input', changed);
    pop.addEventListener('change', changed);
    reset.addEventListener('click', function () {
      state = {};
      show(state);
      F.apply(state);
      clearTimeout(timer);
      persist();
    });
    // the page's own shortcuts (T / O / H / Esc) must not fire while typing here
    ['keydown', 'keyup', 'keypress'].forEach(function (t) {
      pop.addEventListener(t, function (ev) {
        if (t === 'keydown' && ev.key === 'Escape') close(true);
        ev.stopPropagation();
      });
    });
    function open() {
      pop.hidden = false;
      btn.setAttribute('aria-expanded', 'true');
      ctl.fontPreset.focus();
    }
    function close(refocus) {
      if (pop.hidden) return;
      pop.hidden = true;
      btn.setAttribute('aria-expanded', 'false');
      if (refocus) btn.focus();
    }
    btn.addEventListener('click', function () { if (pop.hidden) open(); else close(false); });
    bar.addEventListener('keydown', function (ev) {
      if (ev.key === 'Escape' && !pop.hidden) { close(true); ev.stopPropagation(); }
    });
    document.addEventListener('click', function (ev) {
      if (!pop.hidden && !pop.contains(ev.target) && !btn.contains(ev.target)) close(false);
    });
    // localStorage had prefs but the server had none (older run / other port): hand them to the server once
    if (!serverHad && Object.keys(state).length) safe(persist);
  }

  // ---- sticky summary bar ----
  safe(function () {
    var content = document.getElementById('content');
    if (!content || document.getElementById('pi-bar')) return;
    var bar = document.createElement('div');
    bar.id = 'pi-bar';
    var html = '<a href="/" title="セッション一覧へ">← 一覧に戻る</a>';
    safe(function () {
      if (!data) return;
      var tok = { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 }, cost = 0, hasCost = false;
      var users = 0, asst = 0, tools = 0, models = {}, first = null, last = null;
      data.entries.forEach(function (e) {
        if (e.timestamp) {
          var t = Date.parse(e.timestamp);
          if (!isNaN(t)) { if (first === null || t < first) first = t; if (last === null || t > last) last = t; }
        }
        if (e.type !== 'message' || !e.message) return;
        var m = e.message;
        if (m.role === 'user') users++;
        if (m.role === 'assistant') {
          asst++;
          if (m.model) models[m.model] = 1;
          if (Array.isArray(m.content)) m.content.forEach(function (b) { if (b && b.type === 'toolCall') tools++; });
          var u = m.usage;
          if (u) {
            tok.input += u.input || 0; tok.output += u.output || 0; tok.cacheRead += u.cacheRead || 0; tok.cacheWrite += u.cacheWrite || 0;
            var c = costOf(u);
            if (c !== null) { cost += c; hasCost = true; }
          }
        }
      });
      var h = data.header || {};
      if (h.cwd) {
        var base = String(h.cwd).replace(/[\\/]+$/, '').split(/[\\/]/).pop();
        html += '<span class="pi-item" title="' + esc(h.cwd) + '">📁<b>' + esc(base || h.cwd) + '</b></span>';
      }
      var start = first !== null ? first : Date.parse(h.timestamp);
      if (!isNaN(start)) {
        var span = last !== null ? fmtDur(last - start) : '';
        html += '<span class="pi-item" title="開始〜最終更新">期間<b>' + esc(fmtDate(start)) +
          (last !== null && last - start >= 60000 ? ' 〜 ' + esc(fmtDate(last)) : '') + (span ? '（' + span + '）' : '') + '</b></span>';
      }
      html += '<span class="pi-item" title="user / assistant / tool calls">メッセージ<b>' + users + ' / ' + asst + ' / ' + tools + '</b></span>';
      if (hasCost) html += '<span class="pi-item">コスト<b>' + fmtCost(cost) + '</b></span>';
      html += '<span class="pi-item" title="input / output / cacheRead / cacheWrite">トークン<b>↑' + fmtTok(tok.input) + ' ↓' + fmtTok(tok.output) +
        ' R' + fmtTok(tok.cacheRead) + ' W' + fmtTok(tok.cacheWrite) + '</b></span>';
      var ms = Object.keys(models);
      if (ms.length) html += '<span class="pi-item" title="' + esc(ms.join(', ')) + '">モデル<b>' + esc(ms[0]) + (ms.length > 1 ? ' +' + (ms.length - 1) : '') + '</b></span>';
    });
    html += '<span class="pi-grow"></span>' +
      '<button type="button" data-pi="thinking" title="thinking の全展開/全折りたたみ (T)">thinking</button>' +
      '<button type="button" data-pi="tools" title="ツール出力の全展開/全折りたたみ (O)">tools</button>' +
      (window.piHistoryFont ? '<button type="button" class="pi-fontbtn" aria-haspopup="dialog" aria-expanded="false" aria-controls="pi-font-pop" title="本文フォントの設定">Aa</button>' : '');
    bar.innerHTML = html;
    content.insertBefore(bar, content.firstChild);
    safe(function () { buildFontUI(bar); });

    var map = { thinking: 'toggle-thinking', tools: 'toggle-tools' };
    function real(b) { return document.querySelector('[data-action="' + map[b.getAttribute('data-pi')] + '"]'); }
    function sync() {
      bar.querySelectorAll('button[data-pi]').forEach(function (b) {
        var r = real(b);
        if (r) b.setAttribute('aria-pressed', r.getAttribute('aria-pressed') || 'false');
        else b.style.display = 'none';
      });
    }
    bar.addEventListener('click', function (ev) {
      var b = ev.target.closest && ev.target.closest('button[data-pi]');
      if (!b) return;
      var r = real(b);
      if (r) r.click();
      sync();
    });
    document.addEventListener('keydown', function () { setTimeout(sync, 0); });
    document.addEventListener('click', function () { setTimeout(sync, 0); });
    // thinking is expanded by default in the export; start collapsed (the export keeps this state across re-renders)
    var th = document.querySelector('[data-action="toggle-thinking"]');
    if (th && th.getAttribute('aria-pressed') === 'true') th.click();
    sync();
  });

  // ---- click a thinking block to open/close just that block ----
  safe(function () {
    var msgs = document.getElementById('messages');
    if (!msgs) return;
    msgs.addEventListener('click', function (ev) {
      var tb = ev.target.closest && ev.target.closest('.thinking-block');
      if (!tb) return;
      var sel = window.getSelection && window.getSelection();
      if (sel && String(sel).length) return;
      tb.classList.toggle('pi-open');
    });
  });

  // ---- per-assistant-message usage badge (re-applied when the export re-renders #messages) ----
  safe(function () {
    var msgs = document.getElementById('messages');
    if (!msgs || !data) return;
    var usage = {};
    data.entries.forEach(function (e) {
      if (e.type === 'message' && e.message && e.message.role === 'assistant' && e.message.usage) usage['entry-' + e.id] = e.message;
    });
    function decorate() {
      safe(function () {
        msgs.querySelectorAll('.assistant-message[id]').forEach(function (el) {
          if (el.querySelector('.pi-usage')) return;
          var m = usage[el.id];
          if (!m) return;
          var u = m.usage, c = costOf(u);
          var b = document.createElement('span');
          b.className = 'pi-usage';
          b.title = 'input ' + (u.input || 0) + ' / output ' + (u.output || 0) + ' / cacheRead ' + (u.cacheRead || 0) +
            ' / cacheWrite ' + (u.cacheWrite || 0) + (m.model ? ' · ' + m.model : '');
          b.innerHTML = (c !== null ? '<b>' + fmtCost(c) + '</b> · ' : '') + '↑' + fmtTok(u.input) + ' ↓' + fmtTok(u.output) + ' R' + fmtTok(u.cacheRead);
          var ts = el.querySelector(':scope > .message-timestamp');
          if (ts) ts.appendChild(b);
          else {
            var d = document.createElement('div');
            d.className = 'message-timestamp';
            d.appendChild(b);
            el.insertBefore(d, el.firstChild);
          }
        });
      });
    }
    decorate();
    new MutationObserver(decorate).observe(msgs, { childList: true });
  });
})();
