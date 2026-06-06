// jcode telemetry console — "Terminal Observatory" aesthetic.
//
// Design intent (frontend-design skill): jcode is a terminal coding agent, so
// the dashboard is built as a precision instrument readout, not generic SaaS.
// - Type: JetBrains Mono (display + data) paired with a quiet grotesk for prose.
// - Palette: near-black graphite, warm phosphor amber as the dominant signal,
//   a single cyan accent for the live/headline series. No purple-on-white.
// - Composition: a station-clock hero number, hairline rules, dense tabular
//   instrument panels, scanline texture, staggered load-in reveals.
//
// Self-contained (HTML/CSS/inline-SVG, fonts via Google Fonts <link>). Fetches
// /v1/stats with the dashboard token. Every metric the API returns is shown,
// grouped by importance (HEADLINE / SIGNAL / DIAGNOSTIC).

export const DASHBOARD_HTML = `<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8" />
<meta name="viewport" content="width=device-width, initial-scale=1" />
<title>jcode · telemetry console</title>
<link rel="preconnect" href="https://fonts.googleapis.com">
<link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
<link href="https://fonts.googleapis.com/css2?family=JetBrains+Mono:wght@400;500;700;800&family=Sora:wght@400;500;600&display=swap" rel="stylesheet">
<style>
  :root {
    --bg:        #07090c;
    --bg-grain:  #0a0d12;
    --panel:     #0d1117;
    --panel-2:   #11161e;
    --rule:      #1c232e;
    --rule-soft: #141a22;
    --ink:       #e8eef5;
    --ink-dim:   #9aa7b6;
    --ink-faint: #5c6675;
    --amber:     #ffb454;   /* dominant phosphor signal */
    --amber-dim: #c98a3f;
    --cyan:      #4fd6ff;    /* live / headline accent */
    --green:     #5ad27a;
    --red:       #ff6b6b;
    --mono: "JetBrains Mono", ui-monospace, SFMono-Regular, Menlo, monospace;
    --sans: "Sora", system-ui, sans-serif;
  }
  * { box-sizing: border-box; }
  html, body { margin: 0; padding: 0; }
  body {
    background:
      radial-gradient(900px 500px at 88% -8%, rgba(255,180,84,0.07), transparent 60%),
      radial-gradient(700px 500px at -5% 110%, rgba(79,214,255,0.05), transparent 60%),
      var(--bg);
    color: var(--ink);
    font-family: var(--sans);
    font-size: 14px; line-height: 1.55;
    min-height: 100vh;
    -webkit-font-smoothing: antialiased;
  }
  /* faint scanline texture, instrument vibe */
  body:before {
    content:""; position: fixed; inset: 0; pointer-events: none; z-index: 0;
    background-image: repeating-linear-gradient(0deg, rgba(255,255,255,0.014) 0 1px, transparent 1px 3px);
    mix-blend-mode: overlay; opacity: .5;
  }
  .wrap { position: relative; z-index: 1; max-width: 1200px; margin: 0 auto; padding: 30px 24px 90px; }
  .mono { font-family: var(--mono); }
  .num { font-family: var(--mono); font-variant-numeric: tabular-nums; }

  /* ---- masthead ---- */
  header.bar { display:flex; align-items:center; justify-content:space-between; gap:16px;
    border-bottom: 1px solid var(--rule); padding-bottom: 16px; margin-bottom: 26px; flex-wrap: wrap; }
  .mark { display:flex; align-items:center; gap:13px; }
  .glyph { font-family: var(--mono); font-weight: 800; font-size: 15px; color: var(--bg);
    background: var(--amber); width: 32px; height: 32px; display:grid; place-items:center; border-radius: 7px;
    box-shadow: 0 0 0 1px rgba(255,180,84,.4), 0 0 22px rgba(255,180,84,.25); }
  .mark h1 { font-family: var(--mono); font-size: 14px; font-weight: 700; margin:0; letter-spacing: 1px; text-transform: uppercase; }
  .mark .tag { font-family: var(--mono); font-size: 11px; color: var(--ink-faint); letter-spacing: .5px; }
  .bar-actions { display:flex; align-items:center; gap:10px; }
  .stamp { font-family: var(--mono); font-size: 11px; color: var(--ink-dim); border: 1px solid var(--rule);
    padding: 6px 10px; border-radius: 6px; letter-spacing: .3px; }
  .stamp .live { color: var(--green); }
  .stamp .live:before { content:"●"; margin-right: 6px; animation: pulse 2s ease-in-out infinite; }
  @keyframes pulse { 0%,100%{opacity:1} 50%{opacity:.35} }
  button.btn { cursor:pointer; font-family: var(--mono); font-size: 12px; color: var(--ink); background: var(--panel-2);
    border: 1px solid var(--rule); padding: 7px 13px; border-radius: 6px; letter-spacing: .3px; transition: .15s; }
  button.btn:hover { border-color: var(--amber); color: var(--amber); }

  /* ---- section ---- */
  .sec { margin: 34px 0 14px; display:flex; align-items:baseline; gap: 12px; }
  .sec .idx { font-family: var(--mono); font-size: 11px; color: var(--amber); letter-spacing: 1px; }
  .sec h2 { font-family: var(--mono); font-size: 12px; font-weight: 700; letter-spacing: 2px; text-transform: uppercase; margin: 0; color: var(--ink); }
  .sec .rule { flex:1; height: 1px; background: linear-gradient(90deg, var(--rule), transparent); align-self: center; }
  .sec .note { font-family: var(--mono); font-size: 11px; color: var(--ink-faint); letter-spacing: .3px; }

  /* ---- hero ---- */
  .hero { display:grid; grid-template-columns: 1.05fr 1fr; gap: 1px; background: var(--rule);
    border: 1px solid var(--rule); border-radius: 14px; overflow:hidden; }
  @media (max-width: 880px){ .hero { grid-template-columns: 1fr; } }
  .hero > div { background: var(--panel); }
  .hero-main { padding: 30px 32px; position: relative; }
  .hero-main:after { content:""; position:absolute; inset:0; pointer-events:none;
    background: radial-gradient(420px 220px at 100% 0%, rgba(255,180,84,.10), transparent 70%); }
  .label { font-family: var(--mono); font-size: 11px; letter-spacing: 2px; text-transform: uppercase; color: var(--ink-faint); }
  .big { font-family: var(--mono); font-weight: 800; font-size: clamp(58px, 9vw, 96px); line-height: .92;
    letter-spacing: -2px; color: var(--amber); margin: 8px 0 4px; text-shadow: 0 0 36px rgba(255,180,84,.22); }
  .big .unit { font-size: 18px; color: var(--ink-dim); letter-spacing: 0; margin-left: 10px; text-shadow:none; }
  .hero-desc { color: var(--ink-dim); font-size: 13px; max-width: 48ch; }
  .ladder { margin-top: 22px; border-top: 1px solid var(--rule-soft); }
  .rung { display:flex; align-items:center; justify-content:space-between; padding: 9px 0; border-bottom: 1px solid var(--rule-soft); }
  .rung .lk { font-family: var(--mono); font-size: 12px; color: var(--ink-dim); letter-spacing:.3px; }
  .rung .lk b { color: var(--ink); font-weight: 500; }
  .rung .lv { font-family: var(--mono); font-weight: 700; font-size: 16px; }
  .rung .lv.amber { color: var(--amber); } .rung .lv.cyan { color: var(--cyan); } .rung .lv.dim { color: var(--ink-dim); }
  .hero-side { padding: 24px 26px; display:flex; flex-direction: column; }
  .hero-side h3 { font-family: var(--mono); font-size: 11px; letter-spacing: 1.5px; text-transform: uppercase; color: var(--ink-faint); margin: 0 0 14px; }
  .triple { display:grid; grid-template-columns: repeat(3,1fr); gap: 14px; margin-bottom: 6px; }
  .triple .t .tn { font-family: var(--mono); font-weight: 800; font-size: 30px; letter-spacing: -1px; color: var(--cyan); }
  .triple .t .tl { font-family: var(--mono); font-size: 11px; color: var(--ink-faint); letter-spacing: 1px; text-transform: uppercase; margin-top: 2px; }
  .triple .t .tsub { font-size: 11px; color: var(--ink-faint); }

  /* ---- cards ---- */
  .grid { display:grid; gap: 14px; }
  .g4 { grid-template-columns: repeat(4,1fr); } .g3 { grid-template-columns: repeat(3,1fr); } .g2 { grid-template-columns: repeat(2,1fr); }
  @media (max-width: 1000px){ .g4 { grid-template-columns: repeat(2,1fr); } .g3 { grid-template-columns: repeat(2,1fr); } }
  @media (max-width: 600px){ .g4,.g3,.g2 { grid-template-columns: 1fr; } }
  .stat { background: var(--panel); border: 1px solid var(--rule); border-radius: 11px; padding: 15px 16px; position: relative; overflow: hidden; }
  .stat.key { border-color: rgba(255,180,84,.32); }
  .stat.key:before { content:""; position:absolute; left:0; top:0; bottom:0; width:2px; background: var(--amber); }
  .stat.alert { border-color: rgba(255,107,107,.4); }
  .stat.alert:before { content:""; position:absolute; left:0; top:0; bottom:0; width:2px; background: var(--red); }
  .stat .sl { font-family: var(--mono); font-size: 11px; color: var(--ink-dim); letter-spacing: .4px; display:flex; align-items:center; gap:7px; }
  .stat .sv { font-family: var(--mono); font-weight: 700; font-size: 26px; margin-top: 8px; letter-spacing: -.5px; }
  .stat.key .sv { color: var(--amber); } .stat.alert .sv { color: var(--red); }
  .stat .sm { font-size: 11px; color: var(--ink-faint); margin-top: 3px; }
  .kk { font-family: var(--mono); font-size: 9px; letter-spacing: 1px; color: var(--bg); background: var(--amber); padding: 1px 5px; border-radius: 3px; }

  /* ---- panels w/ tables & charts ---- */
  .panel { background: var(--panel); border: 1px solid var(--rule); border-radius: 12px; padding: 18px 18px 12px; }
  .panel h3 { font-family: var(--mono); font-size: 12px; font-weight: 700; letter-spacing: 1px; text-transform: uppercase; margin: 0 0 3px; }
  .panel .pd { font-size: 11px; color: var(--ink-faint); margin: 0 0 14px; font-family: var(--mono); letter-spacing: .2px; }
  table { width:100%; border-collapse: collapse; }
  th, td { text-align:left; padding: 7px 4px; border-bottom: 1px solid var(--rule-soft); font-size: 12px; }
  th { font-family: var(--mono); color: var(--ink-faint); font-weight: 500; font-size: 10px; letter-spacing: 1px; text-transform: uppercase; }
  td.k { font-family: var(--mono); color: var(--ink); letter-spacing: .2px; }
  td.v, th.v { text-align:right; font-family: var(--mono); font-variant-numeric: tabular-nums; color: var(--ink); }
  .track { height: 6px; background: var(--panel-2); border-radius: 2px; overflow:hidden; }
  .fill { height: 100%; background: linear-gradient(90deg, var(--amber-dim), var(--amber)); border-radius: 2px; }
  tr:last-child td { border-bottom: none; }
  .lb-ci { font-family: var(--mono); font-size: 9px; letter-spacing: 1px; color: var(--bg); background: var(--ink-faint); padding: 1px 5px; border-radius: 3px; margin-left: 6px; }
  .lb-dev { font-family: var(--mono); font-size: 9px; letter-spacing: .5px; color: var(--amber); border: 1px solid rgba(255,180,84,.35); padding: 0 5px; border-radius: 3px; margin-left: 6px; }
  tr.lb-dim td { opacity: .5; }

  .legend { display:flex; gap: 16px; align-items:center; font-family: var(--mono); font-size: 11px; color: var(--ink-dim); margin-bottom: 10px; flex-wrap: wrap; }
  .legend i { width: 14px; height: 3px; display:inline-block; margin-right: 6px; vertical-align: 3px; border-radius: 2px; }

  .fb { padding: 12px 0; border-bottom: 1px solid var(--rule-soft); }
  .fb:last-child { border-bottom: none; }
  .fb .q { color: var(--ink); font-size: 13.5px; }
  .fb .m { font-family: var(--mono); color: var(--ink-faint); font-size: 11px; margin-top: 4px; letter-spacing: .2px; }
  .fb .badge { color: var(--amber); }

  /* ---- gate ---- */
  .gate { max-width: 440px; margin: 16vh auto 0; }
  .gate .box { background: var(--panel); border: 1px solid var(--rule); border-radius: 14px; padding: 30px 28px; text-align: center;
    box-shadow: 0 0 60px rgba(0,0,0,.5); }
  .gate .glyph { margin: 0 auto 16px; width: 40px; height: 40px; font-size: 18px; }
  .gate h2 { font-family: var(--mono); letter-spacing: 1px; margin: 0 0 4px; font-size: 16px; }
  .gate p { color: var(--ink-faint); font-size: 12px; font-family: var(--mono); margin: 0; }
  .gate input { width: 100%; margin: 18px 0 12px; padding: 12px 14px; border-radius: 9px; border: 1px solid var(--rule);
    background: var(--bg); color: var(--ink); font-family: var(--mono); font-size: 13px; letter-spacing: 1px; }
  .gate input:focus { outline: none; border-color: var(--amber); }
  .gate button { width: 100%; padding: 12px; font-weight: 700; }
  .err { color: var(--red); font-family: var(--mono); font-size: 12px; min-height: 18px; margin-top: 6px; }

  .hidden { display:none !important; }
  .foot { font-family: var(--mono); color: var(--ink-faint); font-size: 11px; margin-top: 38px; padding-top: 18px;
    border-top: 1px solid var(--rule-soft); text-align: center; letter-spacing: .2px; line-height: 1.8; }
  .spin { display:inline-block; width: 13px; height: 13px; border: 2px solid var(--rule); border-top-color: var(--amber); border-radius: 50%; animation: sp .7s linear infinite; vertical-align: -2px; }
  @keyframes sp { to { transform: rotate(360deg); } }
  .reveal { animation: rise .5s cubic-bezier(.2,.7,.2,1) both; }
  @keyframes rise { from { opacity: 0; transform: translateY(10px); } to { opacity:1; transform:none; } }
</style>
</head>
<body>
<div class="wrap">
  <div id="gate" class="gate hidden">
    <div class="box">
      <div class="glyph">jc</div>
      <h2>TELEMETRY CONSOLE</h2>
      <p>access token required</p>
      <input id="token" type="password" placeholder="•••••••••••" autocomplete="off" />
      <button class="btn" id="unlock">AUTHENTICATE</button>
      <div class="err" id="gate-err"></div>
    </div>
  </div>

  <div id="app" class="hidden">
    <header class="bar">
      <div class="mark">
        <div class="glyph">jc</div>
        <div>
          <h1>jcode telemetry</h1>
          <div class="tag" id="generated">— · — · —</div>
        </div>
      </div>
      <div class="bar-actions">
        <span class="stamp"><span class="live" id="livestamp">LIVE</span></span>
        <button class="btn" id="refresh">↻ REFRESH</button>
        <button class="btn" id="logout">LOCK</button>
      </div>
    </header>
    <div id="content"></div>
    <div class="foot">
      users are distinct anonymous telemetry_id · headline excludes CI runners &amp; non-release builds<br/>
      raw / CI-inclusive figures retained in diagnostic tier · counts only, no raw events leave the worker
    </div>
  </div>
</div>

<script>
const fmt = (n) => (n==null?"—":Number(n).toLocaleString());
const pct = (x) => (x==null?"—":(x*100).toFixed(1)+"%");
const ms  = (x) => (x==null?"—":x>=1000?(x/1000).toFixed(1)+"s":Math.round(x)+"ms");
const dec = (x,d=1) => (x==null?"—":Number(x).toFixed(d));
const esc = (s) => String(s==null?"":s).replace(/[&<>"]/g,c=>({"&":"&amp;","<":"&lt;",">":"&gt;",'"':"&quot;"}[c]));
let TOKEN = localStorage.getItem("jcode_dash_token") || "";

function showGate(m){ document.getElementById("app").classList.add("hidden"); document.getElementById("gate").classList.remove("hidden"); document.getElementById("gate-err").textContent = m||""; }
function showApp(){ document.getElementById("gate").classList.add("hidden"); document.getElementById("app").classList.remove("hidden"); }

async function load(){
  if(!TOKEN){ showGate(""); return; }
  document.getElementById("content").innerHTML = '<div class="mono" style="padding:60px 0;color:var(--ink-faint)"><span class="spin"></span> reading instruments…</div>';
  showApp();
  let res;
  try { res = await fetch("/v1/stats?token="+encodeURIComponent(TOKEN), { headers:{ "Authorization":"Bearer "+TOKEN } }); }
  catch(e){ showGate("network error"); return; }
  if(res.status===401){ localStorage.removeItem("jcode_dash_token"); TOKEN=""; showGate("invalid token"); return; }
  if(!res.ok){ document.getElementById("content").innerHTML='<div class="err">failed to load ('+res.status+')</div>'; return; }
  render(await res.json());
}

function sec(idx,title,note){ return '<div class="sec reveal"><span class="idx">'+idx+'</span><h2>'+esc(title)+'</h2><span class="rule"></span><span class="note">'+esc(note||"")+'</span></div>'; }
function stat(label,value,meta,opts){ opts=opts||{};
  const cls = opts.alert?'stat alert':(opts.key?'stat key':'stat');
  return '<div class="'+cls+' reveal"><div class="sl">'+esc(label)+(opts.key?' <span class="kk">KEY</span>':'')+'</div><div class="sv">'+value+'</div><div class="sm">'+(meta||'')+'</div></div>';
}
function tablePanel(title,desc,rows,kcol,vcol){
  const max = Math.max(1, ...rows.map(r=>r.value));
  const body = rows.length ? rows.map(r=>'<tr><td class="k">'+esc(r.label)+'</td><td style="width:46%"><div class="track"><div class="fill" style="width:'+Math.max(3,(r.value/max)*100)+'%"></div></div></td><td class="v">'+fmt(r.value)+'</td></tr>').join('') : '<tr><td class="k" colspan="3" style="color:var(--ink-faint)">no data</td></tr>';
  return '<div class="panel reveal"><h3>'+esc(title)+'</h3><p class="pd">'+esc(desc)+'</p><table><thead><tr><th>'+esc(kcol)+'</th><th>·</th><th class="v">'+esc(vcol)+'</th></tr></thead><tbody>'+body+'</tbody></table></div>';
}
function rows(arr,k){ return (arr||[]).map(r=>({label:r[k]??"unknown", value:r.users})); }

function lineChart(series){
  const W=820,H=230,pl=40,pr=14,pt=16,pb=28;
  const dates = series[0]?series[0].points.map(p=>p.date):[];
  if(!dates.length) return '<div class="mono" style="color:var(--ink-faint);padding:18px;font-size:12px">no timeseries yet</div>';
  const maxV = Math.max(1, ...series.flatMap(s=>s.points.map(p=>p.value)));
  const x=i=>pl+(i/Math.max(1,dates.length-1))*(W-pl-pr);
  const y=v=>pt+(1-v/maxV)*(H-pt-pb);
  const grid=[0,.25,.5,.75,1].map(f=>{const gy=pt+f*(H-pt-pb);const val=Math.round(maxV*(1-f));return '<line x1="'+pl+'" y1="'+gy+'" x2="'+(W-pr)+'" y2="'+gy+'" stroke="#161d27"/><text x="4" y="'+(gy+3)+'" fill="#5c6675" font-size="10" font-family="JetBrains Mono">'+val+'</text>';}).join('');
  const area = series.length?(()=>{const s=series[0];const top=s.points.map((p,i)=>(i?'L':'M')+x(i).toFixed(1)+' '+y(p.value).toFixed(1)).join(' ');return '<path d="'+top+' L'+x(s.points.length-1).toFixed(1)+' '+(H-pb)+' L'+pl+' '+(H-pb)+' Z" fill="url(#ag)" opacity=".18"/>';})():'';
  const paths=series.map(s=>{const d=s.points.map((p,i)=>(i?'L':'M')+x(i).toFixed(1)+' '+y(p.value).toFixed(1)).join(' ');return '<path d="'+d+'" fill="none" stroke="'+s.color+'" stroke-width="2" stroke-linejoin="round"/>';}).join('');
  const ticks = dates.length>1?[0,Math.floor(dates.length/2),dates.length-1].map(i=>'<text x="'+x(i)+'" y="'+(H-8)+'" fill="#5c6675" font-size="10" font-family="JetBrains Mono" text-anchor="middle">'+dates[i].slice(5)+'</text>').join(''):'';
  const legend=series.map(s=>'<span><i style="background:'+s.color+'"></i>'+esc(s.name)+'</span>').join('');
  return '<div class="legend">'+legend+'</div><svg viewBox="0 0 '+W+' '+H+'" width="100%" preserveAspectRatio="xMidYMid meet"><defs><linearGradient id="ag" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#4fd6ff"/><stop offset="1" stop-color="#4fd6ff" stop-opacity="0"/></linearGradient></defs>'+grid+area+paths+ticks+'</svg>';
}

function barsChart(title,desc,data,labelFn,color){
  const max=Math.max(1,...data.map(d=>d.v));
  const W=820,H=160,pl=8,pr=8,pb=22,pt=8,n=data.length;
  const bw=(W-pl-pr)/Math.max(1,n);
  const bars=data.map((d,i)=>{const h=(d.v/max)*(H-pt-pb);const bx=pl+i*bw;return '<rect x="'+(bx+1.5).toFixed(1)+'" y="'+(H-pb-h).toFixed(1)+'" width="'+(bw-3).toFixed(1)+'" height="'+h.toFixed(1)+'" rx="1.5" fill="'+color+'"/>';}).join('');
  const labels=data.map((d,i)=> (i%3===0)?'<text x="'+(pl+i*bw+bw/2).toFixed(1)+'" y="'+(H-7)+'" fill="#5c6675" font-size="9" font-family="JetBrains Mono" text-anchor="middle">'+esc(labelFn(d,i))+'</text>':'').join('');
  return '<div class="panel reveal"><h3>'+esc(title)+'</h3><p class="pd">'+esc(desc)+'</p><svg viewBox="0 0 '+W+' '+H+'" width="100%" preserveAspectRatio="xMidYMid meet">'+bars+labels+'</svg></div>';
}

function leaderboardPanel(rows){
  if(!rows.length) return '<div class="panel reveal"><p class="pd">no data</p></div>';
  const body = rows.map((r,i)=>{
    const ci = Number(r.is_ci)===1;
    const chan = esc(r.build_channel||"?");
    const last = r.last_seen ? new Date((r.last_seen||"").replace(" ","T")+"Z").toLocaleDateString() : "—";
    const tag = ci ? '<span class="lb-ci">CI</span>' : (chan==="release" ? '' : '<span class="lb-dev">'+chan+'</span>');
    return '<tr'+(ci?' class="lb-dim"':'')+'>'
      + '<td class="v" style="color:var(--ink-faint)">'+(i+1)+'</td>'
      + '<td class="k">'+esc(r.id_prefix)+'… '+tag+'</td>'
      + '<td class="v">'+fmt(r.sessions)+'</td>'
      + '<td class="v">'+fmt(r.turns)+'</td>'
      + '<td class="v">'+fmt(r.tokens)+'</td>'
      + '<td class="v">'+fmt(r.tool_calls)+'</td>'
      + '<td class="k" style="color:var(--ink-faint)">v'+esc(r.version||"?")+'</td>'
      + '<td class="k" style="color:var(--ink-faint)">'+esc(last)+'</td>'
      + '</tr>';
  }).join('');
  return '<div class="panel reveal"><h3>Top anonymous ids</h3><p class="pd">ranked by lifecycle sessions · CI / non-release tagged · ids are hashed prefixes only</p>'
    + '<table><thead><tr><th class="v">#</th><th>id</th><th class="v">sessions</th><th class="v">turns</th><th class="v">tokens</th><th class="v">tools</th><th>ver</th><th>last seen</th></tr></thead><tbody>'+body+'</tbody></table></div>';
}

function render(d){
  const dt = new Date(d.generated_at);
  document.getElementById("generated").textContent = dt.toISOString().slice(0,10)+" · "+dt.toLocaleTimeString()+" · UTC rollup";
  const c=document.getElementById("content");
  const u=d.users,a=d.active,lc=d.lifecycle,q=d.quality,ret=d.retention,e=d.errors,h=d.health,b=d.breakdowns;
  const ts=d.timeseries.daily||[];
  const series=[
    {name:"headline DAU",color:"#4fd6ff",points:ts.map(r=>({date:r.date,value:r.headline}))},
    {name:"meaningful",color:"#ffb454",points:ts.map(r=>({date:r.date,value:r.meaningful}))},
    {name:"raw / reached",color:"#5c6675",points:ts.map(r=>({date:r.date,value:r.raw}))},
  ];
  let H="";

  // HERO
  H+='<div class="hero reveal">'
    + '<div class="hero-main">'
      + '<div class="label">total users · headline</div>'
      + '<div class="big">'+fmt(u.total_users)+'<span class="unit">people</span></div>'
      + '<div class="hero-desc">Distinct real people who installed jcode or did meaningful work in it. CI runners excluded; each anonymous machine id counts once.</div>'
      + '<div class="ladder">'
        + '<div class="rung"><span class="lk"><b>Reached</b> · launched it at least once</span><span class="lv dim">'+fmt(u.reached_users)+'</span></div>'
        + '<div class="rung"><span class="lk"><b>Total users</b> · installed OR did work</span><span class="lv amber">'+fmt(u.total_users)+'</span></div>'
        + '<div class="rung"><span class="lk"><b>Core</b> · did meaningful work</span><span class="lv cyan">'+fmt(u.core_users)+'</span></div>'
        + '<div class="rung"><span class="lk"><b>Installed</b> · distinct install events</span><span class="lv dim">'+fmt(u.installed_users)+'</span></div>'
      + '</div>'
    + '</div>'
    + '<div class="hero-side">'
      + '<h3>active users · distinct, headline definition</h3>'
      + '<div class="triple">'
        + '<div class="t"><div class="tn">'+fmt(a.dau)+'</div><div class="tl">DAU</div><div class="tsub">'+fmt(a.dau_meaningful)+' mean · '+fmt(a.dau_raw)+' raw</div></div>'
        + '<div class="t"><div class="tn">'+fmt(a.wau)+'</div><div class="tl">WAU</div><div class="tsub">'+fmt(a.wau_meaningful)+' mean · '+fmt(a.wau_raw)+' raw</div></div>'
        + '<div class="t"><div class="tn">'+fmt(a.mau)+'</div><div class="tl">MAU</div><div class="tsub">'+fmt(a.mau_meaningful)+' mean · '+fmt(a.mau_raw)+' raw</div></div>'
      + '</div>'
      + '<div style="margin-top:16px;flex:1">'+lineChart(series)+'</div>'
    + '</div>'
  + '</div>';

  // 01 USER COMPOSITION
  H+=sec("01","user composition","each tier broader than the one below · nothing dropped");
  H+='<div class="grid g4">'
    + stat("Reached", fmt(u.reached_users), "ran jcode ≥1 time (non-CI)")
    + stat("Total users", fmt(u.total_users), "installed OR did work", {key:true})
    + stat("Core users", fmt(u.core_users), "did meaningful work")
    + stat("Installed", fmt(u.installed_users), "distinct install events")
  + '</div>';
  H+='<div class="grid g3" style="margin-top:14px">'
    + stat("CI ids · excluded", fmt(u.ci_ids), "ephemeral runners, filtered")
    + stat("All ids incl. CI + dev", fmt(u.all_ids_including_ci), "raw upper bound, never headline")
    + stat("Install events (raw)", fmt(lc.install_events), fmt(lc.install_ids_noci)+" distinct non-CI")
  + '</div>';

  // 02 ACQUISITION & RETENTION
  H+=sec("02","acquisition & retention","are new users sticking?");
  H+='<div class="grid g4">'
    + stat("D7 retention", pct(ret.d7_retention), (ret.d7_retained||0)+" of "+(ret.d7_cohort||0)+" returned", {key:true})
    + stat("Upgrades", fmt(lc.upgrade_events), "version bumps observed")
    + stat("Multi-session rate", pct(q.multi_session_rate), ">1 session at once")
    + stat("Meaningful sessions 30d", fmt(q.meaningful_sessions_30d), "real-work sessions")
  + '</div>';
  H+='<div class="grid g2" style="margin-top:14px">'
    + '<div class="panel reveal"><h3>daily active users · 60d</h3><p class="pd">headline = meaningful work on release, ex-CI · raw = anyone who launched</p>'+lineChart(series)+'</div>'
    + '<div class="panel reveal"><h3>new installs / day · 60d non-CI</h3><p class="pd">distinct ids whose install landed that day</p>'+lineChart([{name:"installs",color:"#5ad27a",points:(d.timeseries.installs||[]).map(r=>({date:r.date,value:r.installs}))}])+'</div>'
  + '</div>';

  // 03 ENGAGEMENT
  H+=sec("03","engagement quality","30-day · non-CI sessions");
  H+='<div class="grid g4">'
    + stat("Session success", pct(q.success_rate), "ended in success state", {key:true})
    + stat("Avg session", dec(q.avg_session_mins)+" min", "per meaningful session")
    + stat("Avg turns / session", dec(q.avg_turns), "user prompts / session")
    + stat("Abandon rate", pct(q.abandon_rate), "left before first response")
  + '</div>';
  H+='<div class="grid g4" style="margin-top:14px">'
    + stat("Turn success", pct(d.turns.turn_success_rate), "per-turn, 30d")
    + stat("Avg turn time", ms(d.turns.avg_turn_ms), "active duration / turn")
    + stat("Time to first response", ms(q.avg_first_response_ms), "agent responsiveness")
    + stat("Time to first tool success", ms(q.avg_first_tool_success_ms), "first useful tool result")
  + '</div>';
  H+='<div class="grid g2" style="margin-top:14px">'
    + stat("Avg tool latency", ms(q.avg_tool_latency_ms), "per executed tool call")
    + stat("Crash rate", pct(lc.crash_rate)+" · completion "+(lc.lifecycle_completion_ratio==null?"—":lc.lifecycle_completion_ratio), "crash share · (ends+crashes)/starts", {key:true})
  + '</div>';

  // 04 TOKEN USAGE
  const tk = d.tokens||{};
  H+=sec("04","token usage","model token volume · non-CI · cache-aware");
  H+='<div class="grid g4">'
    + stat("Total tokens · 30d", fmt(tk.total_30d), "all token types, last 30d", {key:true})
    + stat("Input · 30d", fmt(tk.input_30d), "prompt tokens sent")
    + stat("Output · 30d", fmt(tk.output_30d), "completion tokens")
    + stat("Cache read · 30d", fmt(tk.cache_read_30d), "served from prompt cache")
  + '</div>';
  H+='<div class="grid g4" style="margin-top:14px">'
    + stat("Cache creation · 30d", fmt(tk.cache_creation_30d), "tokens written to cache")
    + stat("Total tokens · all-time", fmt(tk.total_all), "since telemetry began")
    + stat("Input · all-time", fmt(tk.input_all), "")
    + stat("Output · all-time", fmt(tk.output_all), "")
  + '</div>';

  // 05 AGENT AUTONOMY
  const ag = d.agent||{};
  const activeMs = ag.agent_active_ms||0, modelMs = ag.agent_model_ms||0, toolMs = ag.agent_tool_ms||0, idleMs = ag.session_idle_ms||0, blockedMs = ag.agent_blocked_ms||0;
  const hrs = (x)=> x==null?"—":(x/3600000>=1?(x/3600000).toFixed(1)+"h":(x/60000).toFixed(0)+"m");
  H+=sec("05","agent autonomy","30-day · spawning, delegation & where agent time goes");
  H+='<div class="grid g4">'
    + stat("Spawned agents", fmt(ag.spawned_agents), "sub-agents launched", {key:true})
    + stat("Subagent tasks", fmt(ag.subagent_tasks), fmt(ag.subagent_success)+" succeeded")
    + stat("Swarm tasks", fmt(ag.swarm_tasks), fmt(ag.swarm_success)+" succeeded")
    + stat("Background tasks", fmt(ag.background_tasks), fmt(ag.background_completed)+" completed")
  + '</div>';
  H+='<div class="grid g4" style="margin-top:14px">'
    + stat("User cancellations", fmt(ag.user_cancelled), "user interrupted the agent")
    + stat("Agent active time", hrs(activeMs), "total working time, 30d")
    + stat("Time in model", hrs(modelMs), "thinking / generating")
    + stat("Time in tools", hrs(toolMs), "executing tool calls")
  + '</div>';
  H+='<div class="grid g4" style="margin-top:14px">'
    + stat("Agent blocked time", hrs(blockedMs), "waiting on user / approvals")
    + stat("Session idle time", hrs(idleMs), "no activity")
    + stat("Time to first action", ms(ag.avg_time_to_first_action_ms), "agent's first move")
    + stat("Avg max concurrency", dec(ag.avg_max_concurrent,1), "peak parallel sessions")
  + '</div>';

  // 06 RELIABILITY
  H+=sec("06","reliability","error counts · 30d non-CI · watch for spikes");
  H+='<div class="grid g4">'
    + stat("Provider timeouts", fmt(e.provider_timeout), "", {alert:(e.provider_timeout||0)>0})
    + stat("Rate limited", fmt(e.rate_limited), "")
    + stat("Auth failures", fmt(e.auth_failed), "", {alert:(e.auth_failed||0)>0})
    + stat("Tool / MCP errors", fmt((e.tool_error||0)+(e.mcp_error||0)), fmt(e.tool_error)+" tool · "+fmt(e.mcp_error)+" mcp")
  + '</div>';

  // 07 WHO & WHAT
  H+=sec("07","who & what","distinct users per bucket");
  H+='<div class="grid g2">'
    + tablePanel("Versions","adoption by release (non-CI)", rows(b.versions,"version"), "version","users")
    + tablePanel("Platform","os / arch split", rows(b.arch,"platform"), "platform","users")
  + '</div>';
  H+='<div class="grid g2" style="margin-top:14px">'
    + tablePanel("Providers","meaningful sessions by provider", rows(b.providers,"provider"), "provider","users")
    + tablePanel("Auth method","successful auth by provider", rows(b.auth,"auth_provider"), "provider","users")
  + '</div>';
  H+='<div class="grid g2" style="margin-top:14px">'
    + tablePanel("Build channel","incl. dev/local · release is headline channel", rows(b.channels,"build_channel"), "channel","users")
    + tablePanel("Onboarding funnel","distinct users reaching each step", rows(b.onboarding,"step"), "step","users")
  + '</div>';
  // usage-by-hour bar chart
  const hourData = Array.from({length:24},(_,i)=>{const m=(b.hours||[]).find(r=>Number(r.hour)===i); return {v:m?m.sessions:0, hr:i};});
  H+='<div class="grid g2" style="margin-top:14px">'
    + barsChart("Session starts by UTC hour","when sessions begin (non-CI)", hourData, (d)=>String(d.hr).padStart(2,'0'), "#ffb454")
    + tablePanel("Operating system","os split (non-CI)", rows(b.os,"os"), "os","users")
  + '</div>';

  // 08 FEATURE ADOPTION
  const fr = Object.entries(d.features||{}).map(([k,v])=>({label:k.replace(/_/g," "),value:v})).sort((a,b)=>b.value-a.value);
  const tr = [["https",d.transport.https],["ws reuse",d.transport.ws_reuse],["ws fresh",d.transport.ws_fresh],["native http2",d.transport.native_http2],["cli subprocess",d.transport.cli],["other",d.transport.other]].map(([label,value])=>({label,value:value||0})).sort((a,b)=>b.value-a.value);
  H+=sec("08","feature adoption","distinct users per capability · 30d");
  H+='<div class="grid g2">'
    + tablePanel("Features","users who touched each capability", fr, "feature","users")
    + tablePanel("Transport mix","request transport counts (30d non-CI)", tr, "transport","count")
  + '</div>';

  // 09 USER LEADERBOARD
  H+=sec("09","user leaderboard","most active anonymous ids · by lifecycle volume");
  H+=leaderboardPanel(d.leaderboard||[]);

  // 10 PIPELINE HEALTH (diagnostic)
  H+=sec("10","pipeline health","diagnostic · not product metrics · watch for drift");
  H+='<div class="grid g4">'
    + stat("Lifecycle ids", fmt(h.lifecycle_ids), "distinct ids w/ end/crash")
    + stat("Session-start ids", fmt(h.session_start_ids), "distinct ids that launched")
    + stat("Ends without install", fmt(h.lifecycle_ids_without_install), "id mismatch / pre-install loss", {alert:(h.lifecycle_ids_without_install||0) > (h.lifecycle_ids||0)*0.5})
    + stat("Heaviest single id", fmt(h.max_session_events_one_id), "max session events for one id")
  + '</div>';
  H+='<div class="grid g3" style="margin-top:14px">'
    + stat("Top-5 id session events", fmt(h.top5_session_events), "of "+fmt(h.total_session_events)+" total")
    + stat("Total session events", fmt(h.total_session_events), "ends + crashes, all time")
    + stat("CI ids (30d window)", fmt(a.ci_mau), "filtered from headline")
  + '</div>';

  // 11 FEEDBACK
  if((d.feedback||[]).length){
    H+=sec("11","recent feedback","explicit user submissions");
    H+='<div class="panel reveal">'+d.feedback.map(fb=>'<div class="fb"><div class="q">'+esc(fb.feedback_text)+'</div><div class="m">'+esc(new Date((fb.created_at||"").replace(" ","T")+"Z").toLocaleString())+' · v'+esc(fb.version||"?")+(fb.feedback_rating?' · <span class="badge">'+esc(fb.feedback_rating)+'</span>':'')+(fb.feedback_reason?' · '+esc(fb.feedback_reason):'')+'</div></div>').join('')+'</div>';
  }

  c.innerHTML=H;
}

document.getElementById("unlock").addEventListener("click",()=>{const v=document.getElementById("token").value.trim();if(!v){document.getElementById("gate-err").textContent="enter a token";return;}TOKEN=v;localStorage.setItem("jcode_dash_token",v);load();});
document.getElementById("token").addEventListener("keydown",e=>{if(e.key==="Enter")document.getElementById("unlock").click();});
document.getElementById("refresh").addEventListener("click",load);
document.getElementById("logout").addEventListener("click",()=>{localStorage.removeItem("jcode_dash_token");TOKEN="";showGate("");});
load();
</script>
</body>
</html>`;
