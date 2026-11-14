"""Turns the Claude Design export (site.dc.html) into a static page for GitHub Pages:
plain HTML, the Nocturne stylesheet, and a small vanilla binder instead of the dc runtime."""
import re, sys

src, out = sys.argv[1], sys.argv[2]
html = open(src).read()

body = html[html.index('<x-dc>') + 6: html.index('</x-dc>')]
body = re.sub(r'<helmet>.*?</helmet>', '', body, flags=re.S).strip()
logic = html[html.index('data-dc-script'):]
logic = logic[logic.index('>') + 1: logic.index('</script>')]

def sub(old, new, count=1):
    global body
    assert body.count(old) >= count, old
    body = body.replace(old, new) if count == 0 else body.replace(old, new, count)

# Events and properties become data attributes the binder wires up.
body = body.replace('onClick="', 'data-click="').replace('onChange="', 'data-change="')

# Layout hooks for phone widths.
body = body.replace('style="max-width:1200px; margin:0 auto; padding:', 'class="wrap" style="max-width:1200px; margin:0 auto; padding:')
sub('<nav class="nav" class="wrap"', '<nav class="nav wrap"')
for link in ['#how', '#pair', '#pro', '#download']:
    sub(f'<a href="{link}">', f'<a class="nav-link" href="{link}">')
sub('<div style="flex:none; align-self:flex-end; position:relative; margin:0 auto; width:413px; height:580px; perspective:1800px">',
    '<div class="hero-device dc-live" aria-hidden="true" style="flex:none; align-self:flex-end; position:relative; margin:0 auto; width:413px; height:580px; perspective:1800px">')
sub('<div style="flex:none; position:relative; margin:0 auto">',
    '<div class="dc-live" aria-hidden="true" style="flex:none; position:relative; margin:0 auto">')
sub('<div style="display:flex; flex-direction:column; gap:4px; margin-top:12px; max-width:520px">',
    '<div class="dc-live" style="display:flex; flex-direction:column; gap:4px; margin-top:12px; max-width:520px">')
sub('<div style="display:flex; flex-direction:column; gap:4px; max-width:500px">',
    '<div class="dc-live" style="display:flex; flex-direction:column; gap:4px; max-width:500px">')

# The download box works before (and without) JavaScript: static Mac defaults,
# swapped by the binder through data-b-* attributes.
mac = dict(href='https://github.com/iRunTech/remote-duo/releases/latest/download/RemoteDuoHost-macOS.dmg',
           label='Download for Mac', req='macOS 13 or later · Apple silicon and Intel',
           cmdTitle='Or install from Terminal', cmd='curl -fsSL https://iruntech.github.io/remote-duo/install.sh | sh')
sub('href="{{ dl.href }}"', f'href="{mac["href"]}" data-b-href="{{{{ dl.href }}}}"')
for key in ['label', 'req', 'cmdTitle', 'cmd']:
    sub(f'>{{{{ dl.{key} }}}}<', f' data-b-text="{{{{ dl.{key} }}}}">{mac[key]}<')
sub('>{{ dl.note }}<', ' data-b-text="{{ dl.note }}"><')
sub('<i class="ph {{ copyIcon }}"></i><span>{{ copyLabel }}</span>',
    '<i class="ph ph-copy" data-b-class="ph {{ copyIcon }}"></i><span data-b-text="{{ copyLabel }}">Copy</span>')
sub('checked="{{ isMac }}"', 'checked data-b-checked="{{ isMac }}"')
sub('checked="{{ isWin }}"', 'data-b-checked="{{ isWin }}"')
sub('checked="{{ isLinux }}"', 'data-b-checked="{{ isLinux }}"')
assert '>{{ dl.' not in body and ' href="{{' not in body

# The dc logic class, run by our binder instead of the dc runtime.
logic = logic.replace('class Component extends DCLogic {', 'class Component {\n  props = {};\n  setState(s) { Object.assign(this.state, s); schedule(); }')
logic = logic.replace("state = { t: 0, p: 0, os: 'mac', copied: false };",
                      "state = { t: 0, p: 0, os: detectOS(), copied: false };")
assert 'detectOS' in logic

page = f'''<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Remote Duo — a tiny laptop for any computer</title>
<meta name="description" content="Your iPhone becomes the trackpad, keyboard and live screen for your Mac, Windows PC or Linux computer. Made for iPhone Duo.">
<meta name="apple-itunes-app" content="app-id=6834531434">
<meta property="og:title" content="Remote Duo — a tiny laptop for any computer">
<meta property="og:description" content="Trackpad, keyboard and live screen for your Mac, PC or Linux box, from your iPhone.">
<meta property="og:image" content="https://iruntech.github.io/remote-duo/assets/app-icon.png">
<meta name="theme-color" content="#161826">
<link rel="icon" href="assets/app-icon.png">
<link rel="apple-touch-icon" href="assets/app-icon.png">
<link rel="stylesheet" href="nocturne.css">
<script src="https://unpkg.com/@phosphor-icons/web@2.1.1"></script>
<style>
  html {{ scroll-behavior: smooth; }}
  body {{ margin: 0; background: var(--color-bg); }}
  a {{ color: var(--color-accent-300); }}
  a:hover {{ color: var(--color-accent-200); }}
  sc-for, sc-if {{ display: contents; }}
  /* Animated mockups stay hidden until the script has filled them in. */
  html:not(.dc-ready) .dc-live {{ visibility: hidden; }}
  @media (max-width: 720px) {{
    .wrap {{ padding-left: 16px !important; padding-right: 16px !important; }}
    .nav-link {{ display: none; }}
    #top {{ padding-top: 24px !important; gap: 48px !important; }}
  }}
  @media (max-width: 460px) {{
    .hero-device {{ transform: scale(.82); transform-origin: top center; margin-bottom: -104px !important; }}
  }}
</style>
</head>
<body>
{body}
<script>
function detectOS() {{
  const ua = navigator.userAgent;
  if (/Windows/.test(ua)) return 'win';
  if (/Linux|X11|CrOS/.test(ua) && !/Android/.test(ua)) return 'linux';
  return 'mac';
}}

// A tiny binder for the design's template: {{{{ path }}}} in text and attributes,
// data-b-* for attributes that need a static default, data-click/data-change for
// events, and <sc-for>/<sc-if> blocks.
const TOKEN = /\\{{\\{{\\s*([\\w.]+)\\s*\\}}\\}}/g;
const get = (ctx, path) => path.split('.').reduce((o, k) => o == null ? o : o[k], ctx);
const fill = (str, ctx) => str.replace(TOKEN, (_, p) => {{ const v = get(ctx, p); return v == null ? '' : v; }});
const single = str => {{ const m = /^\\s*\\{{\\{{\\s*([\\w.]+)\\s*\\}}\\}}\\s*$/.exec(str); return m && m[1]; }};

function compile(node) {{
  const ups = [];
  for (const child of [...node.childNodes]) {{
    if (child.nodeType === 3) {{
      if (!child.data.includes('{{{{')) continue;
      const tpl = child.data; let last;
      ups.push(ctx => {{ const s = fill(tpl, ctx); if (s !== last) child.data = last = s; }});
    }} else if (child.nodeType === 1) {{
      ups.push(...element(child));
    }}
  }}
  return ups;
}}

function element(el) {{
  const tag = el.tagName.toLowerCase();
  if (tag === 'sc-for') {{
    const listPath = single(el.getAttribute('list')), as = el.getAttribute('as');
    const tpl = document.createDocumentFragment();
    while (el.firstChild) tpl.appendChild(el.firstChild);
    let rows = [];
    return [ctx => {{
      const items = get(ctx, listPath) || [];
      if (rows.length !== items.length) {{
        el.textContent = '';
        rows = items.map(() => {{
          const frag = tpl.cloneNode(true);
          const holder = document.createElement('sc-if');
          holder.appendChild(frag);
          el.appendChild(holder);
          return compile(holder);
        }});
      }}
      items.forEach((item, i) => {{
        const scope = Object.create(ctx); scope[as] = item;
        rows[i].forEach(u => u(scope));
      }});
    }}];
  }}
  const ups = [];
  if (tag === 'sc-if') {{
    const path = single(el.getAttribute('value'));
    ups.push(ctx => {{ el.style.display = get(ctx, path) ? 'contents' : 'none'; }});
  }}
  for (const attr of [...el.attributes]) {{
    const name = attr.name, val = attr.value;
    if (name === 'data-click' || name === 'data-change') {{
      const path = single(val); let ctx0;
      el.addEventListener(name === 'data-click' ? 'click' : 'change', e => {{ const fn = get(ctx0, path); if (fn) fn(e); }});
      ups.push(ctx => {{ ctx0 = ctx; }});
    }} else if (name.startsWith('data-b-')) {{
      const target = name.slice(7); let last;
      ups.push(ctx => {{
        if (target === 'checked') {{ el.checked = !!get(ctx, single(val)); return; }}
        const s = fill(val, ctx);
        if (s === last) return; last = s;
        if (target === 'text') el.textContent = s; else el.setAttribute(target, s);
      }});
    }} else if (val.includes('{{{{') && tag !== 'sc-if') {{
      let last;
      ups.push(ctx => {{ const s = fill(val, ctx); if (s !== last) el.setAttribute(name, last = s); }});
    }}
  }}
  return ups.concat(compile(el));
}}

let comp, updaters, queued = false;
function schedule() {{
  if (queued || !updaters) return;
  queued = true;
  requestAnimationFrame(() => {{ queued = false; render(); }});
}}
function render() {{ const ctx = comp.renderVals(); updaters.forEach(u => u(ctx)); }}

{logic}

comp = new Component();
const still = matchMedia('(prefers-reduced-motion: reduce)').matches;
if (still) {{ comp.props.autoplay = false; comp.state.t = comp.t = OFF + 9.4; comp.state.p = comp.p = 10.2; }}
updaters = compile(document.body);
render();
document.documentElement.classList.add('dc-ready');
comp.componentDidMount();
</script>
</body>
</html>
'''
open(out, 'w').write(page)
print('wrote', out, len(page))
