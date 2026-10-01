/* @ds-bundle: {"format":4,"namespace":"LatticeSignal","components":[{"name":"Tag"},{"name":"Frame"},{"name":"Countdown"},{"name":"Button"},{"name":"Input"},{"name":"Switch"},{"name":"Checkbox"},{"name":"Radio"},{"name":"Segmented"},{"name":"Textarea"},{"name":"Select"},{"name":"Notice"},{"name":"Progress"},{"name":"Table"},{"name":"Icon"},{"name":"Stripe"}]} */
(function () {
  var React = window.React, h = React.createElement;
  function cx() { return Array.prototype.filter.call(arguments, Boolean).join(' '); }

  // state tones carry their own word; emphasis tones (the cassette palette) take the consumer's word
  var TONES = { prompt: 'PROMPT', warn: 'WARN', error: 'ERROR', ok: 'OK', iris: null, red: null, amber: null, cyan: null, ink: null };

  function Tag({ tone, className, children, ...rest }) {
    var t = Object.prototype.hasOwnProperty.call(TONES, tone) ? tone : 'prompt';
    return h('span', Object.assign({}, rest, { className: cx('ls-tag', 'ls-tag-' + t, className) }),
      children != null ? children : TONES[t]);
  }

  var CORNERS = ['tl', 'tr', 'bl', 'br'];

  function Frame({ variant, className, children, ...rest }) {
    var plain = variant === 'plain';
    return h('div', Object.assign({}, rest, { className: cx('ls-frame', plain && 'ls-frame-plain', className) }),
      plain ? null : CORNERS.map(function (c) {
        return h('span', { key: c, className: 'ls-frame-plus ls-frame-plus-' + c, 'aria-hidden': 'true' });
      }),
      children);
  }

  var UNITS = ['DAYS', 'HOURS', 'MIN', 'SEC'];
  var SPOKEN = ['days', 'hours', 'minutes', 'seconds'];

  // Whole seconds left, rounded up so the display reaches 00 exactly at the target.
  function split(ms) {
    var s = Math.max(0, Math.ceil(ms / 1000));
    return [Math.floor(s / 86400), Math.floor((s % 86400) / 3600), Math.floor((s % 3600) / 60), s % 60];
  }
  function pad(n) { return n < 10 ? '0' + n : String(n); }

  function Countdown({ to, onComplete, labels, className, ...rest }) {
    var units = labels && labels.length === 4 ? labels : UNITS;
    var target = new Date(to).getTime();
    var valid = isFinite(target);
    var [now, setNow] = React.useState(Date.now);
    var done = React.useRef(onComplete);
    done.current = onComplete;

    React.useEffect(function () {
      if (!valid) return;
      var id, fired = false;
      function tick() {
        var n = Date.now();
        setNow(n);
        var left = target - n;
        if (left <= 0) {
          if (!fired) { fired = true; if (done.current) done.current(); }
          return;
        }
        // wake on the next whole-second boundary of the remaining time
        id = setTimeout(tick, left % 1000 || 1000);
      }
      tick();
      return function () { clearTimeout(id); };
    }, [target, valid]);

    var parts = valid ? split(target - now) : null;
    var spoken = parts ? parts.map(function (v, i) { return v + ' ' + (labels ? units[i] : SPOKEN[i]); }).join(', ') : 'no date';
    return h('div', Object.assign({}, rest, { role: 'timer', 'aria-label': spoken, className: cx('ls-countdown', className) }),
      h('div', { className: 'ls-countdown-grid', 'aria-hidden': 'true' },
        units.map(function (u, i) {
          return h('div', { key: i, className: 'ls-countdown-cell' },
            h('span', { className: 'ls-countdown-num' }, parts ? pad(parts[i]) : '--'),
            h('span', { className: 'ls-countdown-unit' }, u));
        })));
  }

  function Button({ variant, type, className, children, ...rest }) {
    var v = variant === 'secondary' ? 'secondary' : 'primary';
    return h('button', Object.assign({ type: type || 'button' }, rest, { className: cx('ls-btn', 'ls-btn-' + v, className) }), children);
  }

  // Input, Textarea and Select share one field: label above, hint or error below, all tied to the control
  // by id so a screen reader reads them with it. Other props go to the control itself.
  function fieldOf(tag, base, defaults) {
    return function ({ label, hint, error, mono, id, className, children, ...rest }) {
      var autoId = React.useId();
      var controlId = id || autoId;
      var noteId = controlId + '-note';
      var note = error || hint;
      var control = h(tag, Object.assign({}, defaults, rest, {
        id: controlId,
        className: cx(base, mono && 'ls-input-mono'),
        'aria-invalid': error ? true : undefined,
        'aria-describedby': note ? noteId : undefined
      }), children);
      return h('div', { className: cx('ls-field', error && 'ls-field-invalid', className) },
        label != null ? h('label', { className: 'ls-field-label', htmlFor: controlId }, label) : null,
        tag === 'select' ? h('span', { className: 'ls-select' }, control) : control,
        note ? h('p', { id: noteId, className: error ? 'ls-field-error' : 'ls-field-hint' }, note) : null);
    };
  }
  var Input = fieldOf('input', 'ls-input', { type: 'text' });
  var Textarea = fieldOf('textarea', 'ls-input ls-textarea', { rows: 4 });
  var SelectField = fieldOf('select', 'ls-input ls-select-control', {});

  // A native select; `options` takes strings or { value, label, disabled }, `placeholder` adds an empty first choice
  function Select({ options, placeholder, children, ...rest }) {
    var opts = (options || []).map(function (o) { return typeof o === 'string' ? { value: o, label: o } : o; });
    var items = children || opts.map(function (o) {
      return h('option', { key: o.value, value: o.value, disabled: o.disabled }, o.label);
    });
    if (placeholder != null) {
      items = [h('option', { key: '', value: '', disabled: true }, placeholder)].concat(items);
      if (rest.value === undefined && rest.defaultValue === undefined) rest.defaultValue = '';
    }
    return h(SelectField, Object.assign({}, rest, { children: items }));
  }

  // controlled with `checked`, or uncontrolled from `defaultChecked`; onChange gets the new boolean
  function Switch({ checked, defaultChecked, onChange, label, className, ...rest }) {
    var controlled = checked !== undefined;
    var [own, setOwn] = React.useState(!!defaultChecked);
    var on = controlled ? !!checked : own;
    function toggle() {
      if (!controlled) setOwn(!on);
      if (onChange) onChange(!on);
    }
    return h('button', Object.assign({ type: 'button' }, rest, {
      role: 'switch', 'aria-checked': on, onClick: toggle, className: cx('ls-switch', className)
    }),
      h('span', { className: 'ls-switch-track', 'aria-hidden': 'true' }, h('span', { className: 'ls-switch-thumb' })),
      label != null ? h('span', { className: 'ls-switch-label' }, label) : null);
  }

  // Checkbox and Radio keep the native input (keyboard, forms, grouping by name) and draw the box beside it
  function choice(type) {
    return function ({ label, className, ...rest }) {
      return h('label', { className: cx('ls-choice', 'ls-' + type, className) },
        h('input', Object.assign({}, rest, { type: type, className: 'ls-choice-input' })),
        h('span', { className: 'ls-choice-box', 'aria-hidden': 'true' }),
        label != null ? h('span', { className: 'ls-choice-label' }, label) : null);
    };
  }
  var Checkbox = choice('checkbox');
  var Radio = choice('radio');

  // One choice from a few, shown as joined segments. Native radios underneath give arrow-key movement and
  // radio semantics; the group takes its name from `label`. Controlled with `value`, or from `defaultValue`.
  function Segmented({ label, options, value, defaultValue, onChange, name, disabled, className, ...rest }) {
    var autoId = React.useId();
    var opts = (options || []).map(function (o) { return typeof o === 'string' ? { value: o, label: o } : o; });
    var controlled = value !== undefined;
    var [own, setOwn] = React.useState(defaultValue !== undefined ? defaultValue : (opts[0] && opts[0].value));
    var current = controlled ? value : own;
    function pick(v) {
      if (!controlled) setOwn(v);
      if (onChange) onChange(v);
    }
    return h('div', Object.assign({}, rest, { className: cx('ls-seg-field', className) }),
      label != null ? h('span', { id: autoId + '-label', className: 'ls-field-label' }, label) : null,
      h('div', { role: 'radiogroup', 'aria-labelledby': label != null ? autoId + '-label' : undefined, className: 'ls-seg' },
        opts.map(function (o) {
          return h('label', { key: o.value, className: 'ls-seg-item' },
            h('input', {
              type: 'radio', className: 'ls-seg-input', name: name || autoId, value: o.value,
              checked: current === o.value, disabled: disabled || o.disabled,
              onChange: function () { pick(o.value); }
            }),
            h('span', { className: 'ls-seg-text' }, o.label));
        })));
  }

  // A one-line message led by a state tag; errors are announced at once, the rest politely
  var NOTICE = { info: ['prompt', 'INFO'], ok: ['ok', 'OK'], warn: ['warn', 'WARN'], error: ['error', 'ERROR'] };
  function Notice({ tone, action, className, children, ...rest }) {
    var t = NOTICE[tone] ? tone : 'info';
    // a plain Frame, so a notice is boxed exactly like any other panel
    return h(Frame, Object.assign({ role: t === 'error' ? 'alert' : 'status' }, rest, { variant: 'plain', className: cx('ls-notice', className) }),
      h(React.Fragment, null,
        h(Tag, { tone: NOTICE[t][0] }, NOTICE[t][1]),
        h('div', { className: 'ls-notice-text' }, children),
        action != null ? h('div', { className: 'ls-notice-action' }, action) : null));
  }

  // A row of square cells lit whole, like a tape deck's level meter; the percentage sits above in mono
  // segments: [{ label, value, series }] — up to four parts, each lighting its own run of cells in series 1–4
  function Progress({ value, max, label, cells, segments, className, ...rest }) {
    var autoId = React.useId();
    var m = max > 0 ? max : 100;
    var n = cells > 0 ? Math.round(cells) : 24;
    var parts = (segments || []).slice(0, 4).map(function (s, i) {
      return { label: s.label, value: Math.max(0, Number(s.value) || 0), series: s.series >= 1 && s.series <= 4 ? Math.round(s.series) : i + 1 };
    });
    var total = parts.length ? parts.reduce(function (a, s) { return a + s.value; }, 0) : Number(value) || 0;
    var v = Math.min(m, Math.max(0, total));
    // the cell each part ends at, by its running total, so rounding never loses or doubles a cell
    var ends = [], run = 0;
    parts.forEach(function (s) { run += s.value; ends.push(Math.round((Math.min(m, run) / m) * n)); });
    var lit = Math.round((v / m) * n);
    var cellsEls = [];
    for (var i = 0, k = 0; i < n; i++) {
      while (k < ends.length && i >= ends[k]) k++;
      var tone = i >= lit ? null : parts.length ? (k < parts.length ? 'ls-progress-s' + parts[k].series : null) : 'ls-progress-lit';
      cellsEls.push(h('span', { key: i, className: cx('ls-progress-cell', tone) }));
    }
    return h('div', Object.assign({}, rest, { className: cx('ls-progress', className) }),
      h('div', { className: 'ls-progress-head' },
        label != null ? h('span', { id: autoId, className: 'ls-field-label' }, label) : h('span'),
        h('span', { className: 'ls-progress-value', 'aria-hidden': 'true' }, Math.round((v / m) * 100) + '%')),
      h('div', {
        className: 'ls-progress-bar', role: 'progressbar',
        'aria-valuemin': 0, 'aria-valuemax': m, 'aria-valuenow': v,
        'aria-labelledby': label != null ? autoId : undefined
      }, cellsEls),
      parts.length ? h('ul', { className: 'ls-progress-legend' }, parts.map(function (s, i) {
        return h('li', { key: i },
          h('span', { className: 'ls-progress-key ls-progress-s' + s.series, 'aria-hidden': 'true' }),
          s.label, ' ', h('span', { className: 'ls-progress-value' }, s.value));
      })) : null);
  }

  var STRIPE_BARS = [0, 1, 2];

  // variant: 'band' (default) | 'line' | 'spine'; a spine wraps its children and runs their full height
  function Stripe({ variant, className, children, ...rest }) {
    var v = variant === 'line' || variant === 'spine' ? variant : 'band';
    var bars = STRIPE_BARS.map(function (i) { return h('span', { key: i, className: 'ls-stripe-bar' }); });
    if (v !== 'spine') return h('div', Object.assign({ 'aria-hidden': 'true' }, rest, { className: cx('ls-stripe', 'ls-stripe-' + v, className) }), bars);
    return h('div', Object.assign({}, rest, { className: cx('ls-stripe', 'ls-stripe-spine', className) }),
      h('span', { className: 'ls-stripe-bars', 'aria-hidden': 'true' }, bars),
      h('div', { className: 'ls-stripe-body' }, children));
  }

  // columns: [{ key, label, align, mono, render }]; rows: plain objects read by column key
  function Table({ columns, rows, caption, rowKey, className, ...rest }) {
    var cols = columns || [];
    return h('table', Object.assign({}, rest, { className: cx('ls-table', className) }),
      caption != null ? h('caption', null, caption) : null,
      h('thead', null, h('tr', null, cols.map(function (c) {
        return h('th', { key: c.key, scope: 'col', className: cx(c.align === 'right' && 'ls-cell-right') }, c.label);
      }))),
      h('tbody', null, (rows || []).map(function (r, i) {
        return h('tr', { key: rowKey ? r[rowKey] : i }, cols.map(function (c) {
          return h('td', { key: c.key, className: cx(c.align === 'right' && 'ls-cell-right', c.mono && 'ls-cell-mono') },
            c.render ? c.render(r) : r[c.key]);
        }));
      })));
  }

  // IBM Carbon icons (Apache-2.0, see assets/Icons/LICENSE.txt): the 32-unit artwork, unchanged,
  // drawn at one of four sizes in the current text colour. Decorative unless given a label.
  var ICONS = {"add":"<path d=\"M17 15 17 8 15 8 15 15 8 15 8 17 15 17 15 24 17 24 17 17 24 17 24 15z\"/>","arrow--left":"<path d=\"M14 26 15.41 24.59 7.83 17 28 17 28 15 7.83 15 15.41 7.41 14 6 4 16 14 26z\"/>","arrow--right":"<path d=\"M18 6 16.57 7.393 24.15 15 4 15 4 17 24.15 17 16.57 24.573 18 26 28 16 18 6z\"/>","bot":"<path d=\"M18 10H20V12H18z\"/><path d=\"M12 10H14V12H12z\"/><path d=\"M26,20H21V18h1a2.0023,2.0023,0,0,0,2-2V12h2V10H24V8a2.0023,2.0023,0,0,0-2-2H20V2H18V6H14V2H12V6H10A2.0023,2.0023,0,0,0,8,8v2H6v2H8v4a2.0023,2.0023,0,0,0,2,2h1v2H6a2.0023,2.0023,0,0,0-2,2v8H6V22H26v8h2V22A2.0023,2.0023,0,0,0,26,20ZM10,8H22v8H10Zm3,10h6v2H13Z\"/>","branch":"<path stroke-width=\"0\" d=\"m20,6c0,1.8587,1.2795,3.4109,3,3.858v4.142c0,1.6543-1.3457,3-3,3h-8c-1.1299,0-2.1617.391-3,1.0256v-8.1676c1.7203-.4471,3-1.9993,3-3.858,0-2.2061-1.7944-4-4-4s-4,1.7939-4,4c0,1.8587,1.2797,3.4108,3,3.858v12.142s0,.142,0,.142c-1.7203.4473-3,1.9997-3,3.858,0,2.2056,1.7944,4,4,4s4-1.7944,4-4c0-1.8583-1.2797-3.4107-3-3.858v-.142c0-1.6543,1.3457-3,3-3h8c2.7568,0,5-2.2432,5-5v-4.142c1.7205-.4471,3-1.9993,3-3.858,0-2.2061-1.7939-4-4-4s-4,1.7939-4,4Zm-14,0c0-1.1025.897-2,2-2s2,.8975,2,2c0,1.1025-.897,2-2,2s-2-.8975-2-2Zm4,20c0,1.103-.897,2-2,2s-2-.897-2-2,.897-2,2-2,2,.897,2,2ZM26,6c0,1.1025-.8975,2-2,2s-2-.8975-2-2c0-1.1025.8975-2,2-2s2,.8975,2,2Z\"/>","calendar":"<path d=\"M26,4h-4V2h-2v2h-8V2h-2v2H6C4.9,4,4,4.9,4,6v20c0,1.1,0.9,2,2,2h20c1.1,0,2-0.9,2-2V6C28,4.9,27.1,4,26,4z M26,26H6V12h20\tV26z M26,10H6V6h4v2h2V6h8v2h2V6h4V10z\"/>","chat":"<path d=\"M17.74,30,16,29l4-7h6a2,2,0,0,0,2-2V8a2,2,0,0,0-2-2H6A2,2,0,0,0,4,8V20a2,2,0,0,0,2,2h9v2H6a4,4,0,0,1-4-4V8A4,4,0,0,1,6,4H26a4,4,0,0,1,4,4V20a4,4,0,0,1-4,4H21.16Z\"/><path d=\"M8 10H24V12H8z\"/><path d=\"M8 16H18V18H8z\"/>","checkmark":"<path d=\"M13 24 4 15 5.414 13.586 13 21.171 26.586 7.586 28 9 13 24z\"/>","chevron--down":"<path d=\"M16 22 6 12 7.4 10.6 16 19.2 24.6 10.6 26 12z\"/>","chevron--left":"<path d=\"M10 16 20 6 21.4 7.4 12.8 16 21.4 24.6 20 26z\"/>","chevron--right":"<path d=\"M22 16 12 26 10.6 24.6 19.2 16 10.6 7.4 12 6z\"/>","chevron--up":"<path d=\"M16 10 26 20 24.6 21.4 16 12.8 7.4 21.4 6 20z\"/>","close":"<path d=\"M17.4141 16 24 9.4141 22.5859 8 16 14.5859 9.4143 8 8 9.4141 14.5859 16 8 22.5859 9.4143 24 16 17.4141 22.5859 24 24 22.5859 17.4141 16z\"/>","cloud":"<path d=\"M16,7h0a7.66,7.66,0,0,1,1.51.15,8,8,0,0,1,6.35,6.34l.26,1.35,1.35.24a5.5,5.5,0,0,1-1,10.92H7.5a5.5,5.5,0,0,1-1-10.92l1.34-.24.26-1.35A8,8,0,0,1,16,7m0-2a10,10,0,0,0-9.83,8.12A7.5,7.5,0,0,0,7.49,28h17a7.5,7.5,0,0,0,1.32-14.88,10,10,0,0,0-7.94-7.94A10.27,10.27,0,0,0,16,5Z\"/>","code":"<path d=\"M31 16 24 23 22.59 21.59 28.17 16 22.59 10.41 24 9 31 16z\"/><path d=\"M1 16 8 9 9.41 10.41 3.83 16 9.41 21.59 8 23 1 16z\"/><path d=\"M5.91 15H26.080000000000002V17H5.91z\" transform=\"rotate(-75 15.996 16)\"/>","commit":"<path d=\"M30,15H21.91A5.9925,5.9925,0,0,0,10.09,15H2v2h8.09A5.9925,5.9925,0,0,0,21.91,17H30ZM16,20a4,4,0,1,1,4-4A4.0045,4.0045,0,0,1,16,20Z\"/>","copy":"<path d=\"M28,10V28H10V10H28m0-2H10a2,2,0,0,0-2,2V28a2,2,0,0,0,2,2H28a2,2,0,0,0,2-2V10a2,2,0,0,0-2-2Z\"/><path d=\"M4,18H2V4A2,2,0,0,1,4,2H18V4H4Z\"/>","data--base":"<path d=\"M24,3H8A2,2,0,0,0,6,5V27a2,2,0,0,0,2,2H24a2,2,0,0,0,2-2V5A2,2,0,0,0,24,3Zm0,2v6H8V5ZM8,19V13H24v6Zm0,8V21H24v6Z\"/><circle cx=\"11\" cy=\"8\" r=\"1\"/><circle cx=\"11\" cy=\"16\" r=\"1\"/><circle cx=\"11\" cy=\"24\" r=\"1\"/>","document":"<path d=\"M25.7,9.3l-7-7C18.5,2.1,18.3,2,18,2H8C6.9,2,6,2.9,6,4v24c0,1.1,0.9,2,2,2h16c1.1,0,2-0.9,2-2V10C26,9.7,25.9,9.5,25.7,9.3\tz M18,4.4l5.6,5.6H18V4.4z M24,28H8V4h8v6c0,1.1,0.9,2,2,2h6V28z\"/><path d=\"M10 22H22V24H10z\"/><path d=\"M10 16H22V18H10z\"/>","download":"<path d=\"M26,24v4H6V24H4v4H4a2,2,0,0,0,2,2H26a2,2,0,0,0,2-2h0V24Z\"/><path d=\"M26 14 24.59 12.59 17 20.17 17 2 15 2 15 20.17 7.41 12.59 6 14 16 24 26 14z\"/>","edit":"<path d=\"M2 26H30V28H2z\"/><path d=\"M25.4,9c0.8-0.8,0.8-2,0-2.8c0,0,0,0,0,0l-3.6-3.6c-0.8-0.8-2-0.8-2.8,0c0,0,0,0,0,0l-15,15V24h6.4L25.4,9z M20.4,4L24,7.6\tl-3,3L17.4,7L20.4,4z M6,22v-3.6l10-10l3.6,3.6l-10,10H6z\"/>","filter":"<path d=\"M18,28H14a2,2,0,0,1-2-2V18.41L4.59,11A2,2,0,0,1,4,9.59V6A2,2,0,0,1,6,4H26a2,2,0,0,1,2,2V9.59A2,2,0,0,1,27.41,11L20,18.41V26A2,2,0,0,1,18,28ZM6,6V9.59l8,8V26h4V17.59l8-8V6Z\"/>","flash":"<path d=\"M11.61,29.92a1,1,0,0,1-.6-1.07L12.83,17H8a1,1,0,0,1-1-1.23l3-13A1,1,0,0,1,11,2H21a1,1,0,0,1,.78.37,1,1,0,0,1,.2.85L20.25,11H25a1,1,0,0,1,.9.56,1,1,0,0,1-.11,1l-13,17A1,1,0,0,1,12,30,1.09,1.09,0,0,1,11.61,29.92ZM17.75,13l2-9H11.8L9.26,15h5.91L13.58,25.28,23,13Z\"/>","folder":"<path d=\"M11.17,6l3.42,3.41.58.59H28V26H4V6h7.17m0-2H4A2,2,0,0,0,2,6V26a2,2,0,0,0,2,2H28a2,2,0,0,0,2-2V10a2,2,0,0,0-2-2H16L12.59,4.59A2,2,0,0,0,11.17,4Z\"/>","grid":"<path d=\"M12,4H6A2,2,0,0,0,4,6v6a2,2,0,0,0,2,2h6a2,2,0,0,0,2-2V6A2,2,0,0,0,12,4Zm0,8H6V6h6Z\"/><path d=\"M26,4H20a2,2,0,0,0-2,2v6a2,2,0,0,0,2,2h6a2,2,0,0,0,2-2V6A2,2,0,0,0,26,4Zm0,8H20V6h6Z\"/><path d=\"M12,18H6a2,2,0,0,0-2,2v6a2,2,0,0,0,2,2h6a2,2,0,0,0,2-2V20A2,2,0,0,0,12,18Zm0,8H6V20h6Z\"/><path d=\"M26,18H20a2,2,0,0,0-2,2v6a2,2,0,0,0,2,2h6a2,2,0,0,0,2-2V20A2,2,0,0,0,26,18Zm0,8H20V20h6Z\"/>","information--square":"<path d=\"M17 22 17 14 13 14 13 16 15 16 15 22 12 22 12 24 20 24 20 22 17 22z\"/><path d=\"M16,8a1.5,1.5,0,1,0,1.5,1.5A1.5,1.5,0,0,0,16,8Z\"/><path d=\"M26,28H6a2.0023,2.0023,0,0,1-2-2V6A2.0023,2.0023,0,0,1,6,4H26a2.0023,2.0023,0,0,1,2,2V26A2.0023,2.0023,0,0,1,26,28ZM6,6V26H26V6Z\"/>","launch":"<path d=\"M26,28H6a2.0027,2.0027,0,0,1-2-2V6A2.0027,2.0027,0,0,1,6,4H16V6H6V26H26V16h2V26A2.0027,2.0027,0,0,1,26,28Z\"/><path d=\"M20 2 20 4 26.586 4 18 12.586 19.414 14 28 5.414 28 12 30 12 30 2 20 2z\"/>","link":"<path d=\"M29.25,6.76a6,6,0,0,0-8.5,0l1.42,1.42a4,4,0,1,1,5.67,5.67l-8,8a4,4,0,1,1-5.67-5.66l1.41-1.42-1.41-1.42-1.42,1.42a6,6,0,0,0,0,8.5A6,6,0,0,0,17,25a6,6,0,0,0,4.27-1.76l8-8A6,6,0,0,0,29.25,6.76Z\"/><path d=\"M4.19,24.82a4,4,0,0,1,0-5.67l8-8a4,4,0,0,1,5.67,0A3.94,3.94,0,0,1,19,14a4,4,0,0,1-1.17,2.85L15.71,19l1.42,1.42,2.12-2.12a6,6,0,0,0-8.51-8.51l-8,8a6,6,0,0,0,0,8.51A6,6,0,0,0,7,28a6.07,6.07,0,0,0,4.28-1.76L9.86,24.82A4,4,0,0,1,4.19,24.82Z\"/>","list":"<path d=\"M10 6H28V8H10z\"/><path d=\"M10 24H28V26H10z\"/><path d=\"M10 15H28V17H10z\"/><path d=\"M4 15H6V17H4z\"/><path d=\"M4 6H6V8H4z\"/><path d=\"M4 24H6V26H4z\"/>","locked":"<path d=\"M24,14H22V8A6,6,0,0,0,10,8v6H8a2,2,0,0,0-2,2V28a2,2,0,0,0,2,2H24a2,2,0,0,0,2-2V16A2,2,0,0,0,24,14ZM12,8a4,4,0,0,1,8,0v6H12ZM24,28H8V16H24Z\"/>","overflow-menu--horizontal":"<circle cx=\"8\" cy=\"16\" r=\"2\"/><circle cx=\"16\" cy=\"16\" r=\"2\"/><circle cx=\"24\" cy=\"16\" r=\"2\"/>","pause":"<path d=\"M12,8V24H8V8h4m0-2H8A2,2,0,0,0,6,8V24a2,2,0,0,0,2,2h4a2,2,0,0,0,2-2V8a2,2,0,0,0-2-2Z\"/><path d=\"M24,8V24H20V8h4m0-2H20a2,2,0,0,0-2,2V24a2,2,0,0,0,2,2h4a2,2,0,0,0,2-2V8a2,2,0,0,0-2-2Z\"/>","play":"<path d=\"M7,28a1,1,0,0,1-1-1V5a1,1,0,0,1,1.4819-.8763l20,11a1,1,0,0,1,0,1.7525l-20,11A1.0005,1.0005,0,0,1,7,28ZM8,6.6909V25.3088L24.9248,16Z\"/>","renew":"<path d=\"M12,10H6.78A11,11,0,0,1,27,16h2A13,13,0,0,0,6,7.68V4H4v8h8Z\"/><path d=\"M20,22h5.22A11,11,0,0,1,5,16H3a13,13,0,0,0,23,8.32V28h2V20H20Z\"/>","restart":"<path d=\"M26,18A10,10,0,1,1,16,8h6.1821l-3.5844,3.5854L20,13l6-6L20,1,18.5977,2.414,22.1851,6H16A12,12,0,1,0,28,18Z\"/>","save":"<path d=\"M27.71,9.29l-5-5A1,1,0,0,0,22,4H6A2,2,0,0,0,4,6V26a2,2,0,0,0,2,2H26a2,2,0,0,0,2-2V10A1,1,0,0,0,27.71,9.29ZM12,6h8v4H12Zm8,20H12V18h8Zm2,0V18a2,2,0,0,0-2-2H12a2,2,0,0,0-2,2v8H6V6h4v4a2,2,0,0,0,2,2h8a2,2,0,0,0,2-2V6.41l4,4V26Z\"/>","search":"<path d=\"M29,27.5859l-7.5521-7.5521a11.0177,11.0177,0,1,0-1.4141,1.4141L27.5859,29ZM4,13a9,9,0,1,1,9,9A9.01,9.01,0,0,1,4,13Z\"/>","send":"<path d=\"M27.45,15.11l-22-11a1,1,0,0,0-1.08.12,1,1,0,0,0-.33,1L7,16,4,26.74A1,1,0,0,0,5,28a1,1,0,0,0,.45-.11l22-11a1,1,0,0,0,0-1.78Zm-20.9,10L8.76,17H18V15H8.76L6.55,6.89,24.76,16Z\"/>","settings":"<path d=\"M27,16.76c0-.25,0-.5,0-.76s0-.51,0-.77l1.92-1.68A2,2,0,0,0,29.3,11L26.94,7a2,2,0,0,0-1.73-1,2,2,0,0,0-.64.1l-2.43.82a11.35,11.35,0,0,0-1.31-.75l-.51-2.52a2,2,0,0,0-2-1.61H13.64a2,2,0,0,0-2,1.61l-.51,2.52a11.48,11.48,0,0,0-1.32.75L7.43,6.06A2,2,0,0,0,6.79,6,2,2,0,0,0,5.06,7L2.7,11a2,2,0,0,0,.41,2.51L5,15.24c0,.25,0,.5,0,.76s0,.51,0,.77L3.11,18.45A2,2,0,0,0,2.7,21L5.06,25a2,2,0,0,0,1.73,1,2,2,0,0,0,.64-.1l2.43-.82a11.35,11.35,0,0,0,1.31.75l.51,2.52a2,2,0,0,0,2,1.61h4.72a2,2,0,0,0,2-1.61l.51-2.52a11.48,11.48,0,0,0,1.32-.75l2.42.82a2,2,0,0,0,.64.1,2,2,0,0,0,1.73-1L29.3,21a2,2,0,0,0-.41-2.51ZM25.21,24l-3.43-1.16a8.86,8.86,0,0,1-2.71,1.57L18.36,28H13.64l-.71-3.55a9.36,9.36,0,0,1-2.7-1.57L6.79,24,4.43,20l2.72-2.4a8.9,8.9,0,0,1,0-3.13L4.43,12,6.79,8l3.43,1.16a8.86,8.86,0,0,1,2.71-1.57L13.64,4h4.72l.71,3.55a9.36,9.36,0,0,1,2.7,1.57L25.21,8,27.57,12l-2.72,2.4a8.9,8.9,0,0,1,0,3.13L27.57,20Z\"/><path d=\"M16,22a6,6,0,1,1,6-6A5.94,5.94,0,0,1,16,22Zm0-10a3.91,3.91,0,0,0-4,4,3.91,3.91,0,0,0,4,4,3.91,3.91,0,0,0,4-4A3.91,3.91,0,0,0,16,12Z\"/>","stop":"<path d=\"M24,8V24H8V8H24m0-2H8A2,2,0,0,0,6,8V24a2,2,0,0,0,2,2H24a2,2,0,0,0,2-2V8a2,2,0,0,0-2-2Z\"/>","subtract":"<path d=\"M8 15H24V17H8z\"/>","terminal":"<path d=\"M26,4H6A2,2,0,0,0,4,6V26a2,2,0,0,0,2,2H26a2,2,0,0,0,2-2V6A2,2,0,0,0,26,4Zm0,2v4H6V6ZM6,26V12H26V26Z\" transform=\"translate(0 .01)\"/><path d=\"M10.76 16.18 13.58 19.01 10.76 21.84 12.17 23.25 16.41 19.01 12.17 14.77 10.76 16.18z\"/>","time":"<path d=\"M16,30A14,14,0,1,1,30,16,14,14,0,0,1,16,30ZM16,4A12,12,0,1,0,28,16,12,12,0,0,0,16,4Z\"/><path d=\"M20.59 22 15 16.41 15 7 17 7 17 15.58 22 20.59 20.59 22z\"/>","tool-kit":"<path d=\"M8.9141,24.5l4.257-4.2568-1.414-1.4141L7.5,23.0859l-.793-.7929a.9994.9994,0,0,0-1.414,0l-4,4a.9994.9994,0,0,0,0,1.414l3,3a.9995.9995,0,0,0,1.414,0l4-4a.9994.9994,0,0,0,0-1.414ZM5,28.5859,3.4141,27,6,24.4141,7.5859,26Z\"/><path d=\"M24,30a6.0067,6.0067,0,0,1-6-6,5.84,5.84,0,0,1,.2109-1.5469l-8.664-8.6638A5.8483,5.8483,0,0,1,8,14,5.9757,5.9757,0,0,1,2.4228,5.8164l.5577-1.4219L6.293,7.707a1.0233,1.0233,0,0,0,1.4135,0,.999.999,0,0,0,0-1.4141L4.3936,2.979l1.4233-.5571A5.9772,5.9772,0,0,1,14,8a5.84,5.84,0,0,1-.2109,1.5469l8.664,8.6635A5.8548,5.8548,0,0,1,24,18a5.9755,5.9755,0,0,1,5.5771,8.1836L29.02,27.6055,25.707,24.293a1.0233,1.0233,0,0,0-1.4135,0,.999.999,0,0,0-.0005,1.4141L27.6055,29.02l-1.4219.5579A5.96,5.96,0,0,1,24,30ZM10.0625,11.4763,20.5234,21.9375l-.2392.6094A3.9754,3.9754,0,0,0,23.75,27.9922l-.8711-.8711a2.9992,2.9992,0,0,1,0-4.2424,3.0721,3.0721,0,0,1,4.2427.0005l.8706.8708a3.9759,3.9759,0,0,0-5.4458-3.4658l-.6094.2385-10.46-10.46.2392-.6094A3.9755,3.9755,0,0,0,8.2505,4.0078l.8706.8711a2.9992,2.9992,0,0,1,0,4.2424,3.0721,3.0721,0,0,1-4.2427-.0005L4.0078,8.25a3.975,3.975,0,0,0,5.4453,3.4656Z\"/><path d=\"M29.1226,2.85a3.0716,3.0716,0,0,0-4.2422,0L17.4,10.33l1.4141,1.414,7.48-7.48a1.0244,1.0244,0,0,1,1.4141,0,1.002,1.002,0,0,1,0,1.4145l-7.48,7.48,1.414,1.4141,7.48-7.4795A3.0031,3.0031,0,0,0,29.1226,2.85Z\"/>","trash-can":"<path d=\"M12 12H14V24H12z\"/><path d=\"M18 12H20V24H18z\"/><path d=\"M4,6V8H6V28a2,2,0,0,0,2,2H24a2,2,0,0,0,2-2V8h2V6ZM8,28V8H24V28Z\"/><path d=\"M12 2H20V4H12z\"/>","unlocked":"<path d=\"M24,14H12V8a4,4,0,0,1,8,0h2A6,6,0,0,0,10,8v6H8a2,2,0,0,0-2,2V28a2,2,0,0,0,2,2H24a2,2,0,0,0,2-2V16A2,2,0,0,0,24,14Zm0,14H8V16H24Z\"/>","upload":"<path d=\"M6 18 7.41 19.41 15 11.83 15 30 17 30 17 11.83 24.59 19.41 26 18 16 8 6 18z\"/><path d=\"M6,8V4H26V8h2V4a2,2,0,0,0-2-2H6A2,2,0,0,0,4,4V8Z\"/>","view":"<path d=\"M30.94,15.66A16.69,16.69,0,0,0,16,5,16.69,16.69,0,0,0,1.06,15.66a1,1,0,0,0,0,.68A16.69,16.69,0,0,0,16,27,16.69,16.69,0,0,0,30.94,16.34,1,1,0,0,0,30.94,15.66ZM16,25c-5.3,0-10.9-3.93-12.93-9C5.1,10.93,10.7,7,16,7s10.9,3.93,12.93,9C26.9,21.07,21.3,25,16,25Z\"/><path d=\"M16,10a6,6,0,1,0,6,6A6,6,0,0,0,16,10Zm0,10a4,4,0,1,1,4-4A4,4,0,0,1,16,20Z\"/>","view--off":"<path d=\"M5.24,22.51l1.43-1.42A14.06,14.06,0,0,1,3.07,16C5.1,10.93,10.7,7,16,7a12.38,12.38,0,0,1,4,.72l1.55-1.56A14.72,14.72,0,0,0,16,5,16.69,16.69,0,0,0,1.06,15.66a1,1,0,0,0,0,.68A16,16,0,0,0,5.24,22.51Z\"/><path d=\"M12,15.73a4,4,0,0,1,3.7-3.7l1.81-1.82a6,6,0,0,0-7.33,7.33Z\"/><path d=\"M30.94,15.66A16.4,16.4,0,0,0,25.2,8.22L30,3.41,28.59,2,2,28.59,3.41,30l5.1-5.1A15.29,15.29,0,0,0,16,27,16.69,16.69,0,0,0,30.94,16.34,1,1,0,0,0,30.94,15.66ZM20,16a4,4,0,0,1-6,3.44L19.44,14A4,4,0,0,1,20,16Zm-4,9a13.05,13.05,0,0,1-6-1.58l2.54-2.54a6,6,0,0,0,8.35-8.35l2.87-2.87A14.54,14.54,0,0,1,28.93,16C26.9,21.07,21.3,25,16,25Z\"/>","warning--alt":"<path d=\"M16,23a1.5,1.5,0,1,0,1.5,1.5A1.5,1.5,0,0,0,16,23Z\"/><path d=\"M15 12H17V21H15z\"/><path d=\"M29,30H3a1,1,0,0,1-.8872-1.4614l13-25a1,1,0,0,1,1.7744,0l13,25A1,1,0,0,1,29,30ZM4.6507,28H27.3493l.002-.0033L16.002,6.1714h-.004L4.6487,27.9967Z\"/>"};
  var ICON_SIZES = [16, 20, 24, 32];
  function Icon({ name, size, label, className, ...rest }) {
    var body = ICONS[name];
    if (!body) return null;
    var s = ICON_SIZES.indexOf(size) >= 0 ? size : 20;
    return h('svg', Object.assign({}, rest, {
      className: cx('ls-icon', className), width: s, height: s, viewBox: '0 0 32 32', fill: 'currentColor', focusable: 'false',
      role: label ? 'img' : undefined, 'aria-label': label || undefined, 'aria-hidden': label ? undefined : 'true',
      dangerouslySetInnerHTML: { __html: body }
    }));
  }
  Icon.names = Object.keys(ICONS);

  window.LatticeSignal = Object.assign(window.LatticeSignal || {}, {
    Tag: Tag, Frame: Frame, Countdown: Countdown,
    Button: Button, Input: Input, Switch: Switch, Checkbox: Checkbox, Radio: Radio, Segmented: Segmented,
    Textarea: Textarea, Select: Select, Notice: Notice, Progress: Progress, Table: Table, Icon: Icon, Stripe: Stripe
  });
})();
