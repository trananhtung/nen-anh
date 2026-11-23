// Drives the real window through WebView2's debugging port: drop a folder, press the button,
// wait for the results. Usage: node scripts/e2e.mjs <folder>
const folder = process.argv[2];
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function page() {
  for (let i = 0; i < 60; i++) {
    try {
      const list = await (await fetch("http://127.0.0.1:9222/json")).json();
      const found = list.find((t) => t.type === "page" && t.url.includes("tauri"));
      if (found) return found.webSocketDebuggerUrl;
    } catch {}
    await sleep(500);
  }
  throw new Error("window never appeared");
}

const ws = new WebSocket(await page());
await new Promise((ok, no) => {
  ws.onopen = ok;
  ws.onerror = no;
});
let id = 0;
const waiting = new Map();
ws.onmessage = (m) => {
  const d = JSON.parse(m.data);
  waiting.get(d.id)?.(d);
};
async function js(expression) {
  const n = ++id;
  const reply = new Promise((r) => waiting.set(n, r));
  ws.send(
    JSON.stringify({ id: n, method: "Runtime.evaluate", params: { expression, awaitPromise: true, returnByValue: true } }),
  );
  const { result } = await reply;
  if (result.exceptionDetails) throw new Error(JSON.stringify(result.exceptionDetails));
  return result.result.value;
}
async function until(expression, what, seconds = 90) {
  for (let i = 0; i < seconds * 2; i++) {
    if (await js(expression)) return;
    await sleep(500);
  }
  throw new Error(`${what}; page shows: ${await js("document.body.innerText")}`);
}

await until("!!document.querySelector('.drop')", "start screen missing");
const payload = { paths: [folder], position: { x: 10, y: 10 } };
await js(`window.__TAURI_INTERNALS__.invoke('plugin:event|emit_to', {
  target: { kind: 'Webview', label: 'main' }, event: 'tauri://drag-drop', payload: ${JSON.stringify(payload)} })`);
await until("document.querySelectorAll('.row').length === 2", "dropped folder was not listed");
await js("document.querySelector('.big').click()");
await until("document.querySelector('.big').textContent.includes('Mở thư mục')", "compression did not finish");
const states = await js("[...document.querySelectorAll('.row')].map(r => r.dataset.state).sort().join()");
if (states !== "done,failed") throw new Error(`unexpected row states: ${states}`);
console.log(await js("document.body.innerText"));
ws.close();
