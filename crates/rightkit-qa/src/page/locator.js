const QA = window.__rightkitQaLocator || (window.__rightkitQaLocator = (() => {
  const roleOf = (e) => e.getAttribute('role')?.trim().split(/\s+/)[0] || ({BUTTON:'button',H1:'heading',H2:'heading',H3:'heading',H4:'heading',H5:'heading',H6:'heading',SELECT:'combobox',TEXTAREA:'textbox',SUMMARY:'button',NAV:'navigation',ASIDE:'complementary',DIALOG:'dialog',ARTICLE:'article',UL:'list',LI:'listitem'})[e.tagName]
    || (e.tagName === 'A' && e.hasAttribute('href') ? 'link' : null)
    || (e.tagName === 'INPUT' ? ({checkbox:'checkbox',radio:'radio',range:'slider',button:'button',submit:'button',reset:'button',number:'spinbutton',search:'searchbox',hidden:null})[e.type] ?? (e.type === 'hidden' ? null : 'textbox') : null);
  const labelled = (e) => { const id = e.getAttribute('aria-labelledby'); if (!id) return ''; return id.split(/\s+/).map(i => document.getElementById(i)?.textContent || '').join(' '); };
  const labelFor = (e) => { if (e.labels && e.labels.length) return Array.from(e.labels).map(l => l.innerText).join(' '); return ''; };
  const name = (e) => (labelled(e) || e.getAttribute('aria-label') || labelFor(e) || e.innerText || e.value || e.getAttribute('title') || e.placeholder || '').replace(/\s+/g, ' ').trim();
  const visible = (e) => { if (e.closest('[hidden], [aria-hidden="true"], [inert]')) return false; for (let p = e; p; p = p.parentElement) { const s = getComputedStyle(p); if (s.display === 'none' || s.visibility === 'hidden' || s.visibility === 'collapse' || Number(s.opacity) === 0) return false; } const r = e.getBoundingClientRect(); if (r.width <= 0 || r.height <= 0) return false; const s = getComputedStyle(e); return s.visibility !== 'hidden' && s.display !== 'none' && s.opacity !== '0'; };
  const match = (text, want, exact) => exact ? text === want : (want instanceof RegExp ? (want.lastIndex = 0, want.test(text)) : text.includes(want));
  const all = (role, want, exact, scope) => Array.from((scope ? scope.querySelectorAll('*') : [])).filter(e => roleOf(e) === role && (want == null || match(name(e), want, exact)));
  const scopeOf = (s) => s ? (typeof s === 'string' ? document.querySelector(s) : s) : document;
  let n = 0;
  return {
    roleOf, name, visible,
    byRole: (role, want, exact, scope) => all(role, want, exact, scopeOf(scope)).filter(visible),
    byText: (want, exact, scope) => {
      const els = Array.from((scopeOf(scope)?.querySelectorAll('*') || [])).filter(e => visible(e) && match((e.innerText || '').replace(/\s+/g,' ').trim(), want, exact));
      return els.filter(e => !els.some(o => o !== e && e.contains(o)));
    },
    tag: (e) => { const id = 'qa' + (++n); e.setAttribute('data-rightkit-qa', id); return '[data-rightkit-qa="' + id + '"]'; },
  };
})());
