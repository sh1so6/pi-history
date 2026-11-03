/* pi-history: tiny script placed in <head> (right after the saved-prefs style element (#pi-prefs), if the server has any).
   Owns the font-preference helpers shared with inject.js and, when the server has no saved prefs, applies the
   localStorage copy before first paint. Any failure leaves the export's own look. */
(function () {
  'use strict';
  try {
    var KEY = 'pi-history.sessionFont.v1';
    var VARS = { font: '--ph-font', size: '--ph-font-size', line: '--ph-line', mono: '--ph-mono', monoSize: '--ph-mono-size' };
    var RANGE = { size: [12, 24, 0.5], line: [1.3, 2.2, 0.05], monoSize: [10, 24, 0.5] };

    // same rules as prefs.rs: letters/digits, space , - _ . and balanced quotes only
    function fam(s) {
      if (typeof s !== 'string') return undefined;
      s = s.trim();
      if (!s || s.length > 200) return undefined;
      var q = null;
      for (var i = 0; i < s.length; i++) {
        var c = s.charAt(i);
        if (c === '"' || c === "'") { if (q === c) q = null; else if (q === null) q = c; }
        else if (!/^[\p{L}\p{N}\p{M} ,._-]$/u.test(c)) return undefined;
      }
      return q === null ? s : undefined;
    }
    function num(v, r) {
      if (typeof v !== 'number' || !isFinite(v) || v < r[0] || v > r[1]) return undefined;
      return Math.round(Math.round(v / r[2]) * r[2] * 100) / 100;
    }
    function clean(p) {
      var o = {};
      if (!p || typeof p !== 'object') return o;
      ['fontPreset', 'font', 'monoPreset', 'mono'].forEach(function (k) { var v = fam(p[k]); if (v !== undefined) o[k] = v; });
      ['size', 'line', 'monoSize'].forEach(function (k) { var v = num(p[k], RANGE[k]); if (v !== undefined) o[k] = v; });
      return o;
    }
    function values(p) {
      var v = {};
      var f = p.font || p.fontPreset, m = p.mono || p.monoPreset;
      if (f) v.font = f + ',var(--pi-sans)';
      if (m) v.mono = m + ',var(--pi-mono)';
      if (p.size !== undefined) v.size = p.size + 'px';
      if (p.line !== undefined) v.line = String(p.line);
      if (p.monoSize !== undefined) v.monoSize = p.monoSize + 'px';
      return v;
    }
    function apply(p) {
      var s = document.getElementById('pi-prefs'); // the server-embedded copy; ours (inline) takes over from now on
      if (s && s.parentNode) s.parentNode.removeChild(s);
      var st = document.documentElement.style, v = values(clean(p));
      Object.keys(VARS).forEach(function (k) {
        if (v[k] !== undefined) st.setProperty(VARS[k], v[k]); else st.removeProperty(VARS[k]);
      });
    }
    function readLocal() {
      try { return clean(JSON.parse(localStorage.getItem(KEY))); } catch (e) { return {}; }
    }
    window.piHistoryFont = { KEY: KEY, clean: clean, apply: apply, readLocal: readLocal };
    if (!document.getElementById('pi-prefs') && !document.getElementById('pi-prefs-json')) apply(readLocal());
  } catch (e) {
    try { console.warn('[pi-history]', e); } catch (_) {}
  }
})();
