// Durable native acceptance: public ExplorerKit host API + unchanged standard driver.
// Run after changes to automation, native lifetimes/dialogs, WRY, driver pin or OS.
import { spawn } from 'node:child_process';
import { createInterface } from 'node:readline';
import { mkdir, writeFile } from 'node:fs/promises';
import { resolve } from 'node:path';

const [hostPath, helperPath] = process.argv.slice(2);
if (!hostPath || !helperPath) throw new Error('Usage: bun client.ts HOST MINIDRIVER');
const records: unknown[] = [];
const checks: string[] = [];
const observations: unknown[] = [];
const record = (kind: string, data: unknown) => records.push({ time: new Date().toISOString(), kind, data });
function check(condition: unknown, name: string, detail?: unknown): asserts condition {
  record('check', { name, passed: !!condition, detail });
  if (!condition) throw new Error(`${name}: ${JSON.stringify(detail)}`);
  checks.push(name);
  console.log(`PASS ${name}`);
}
const fixture = Bun.serve({ hostname: '127.0.0.1', port: 0, fetch(request) {
  const name = new URL(request.url).pathname.slice(1);
  if (!/^[A-Za-z0-9_-]+$/.test(name)) return new Response('Not found', { status: 404 });
  return new Response(`<!doctype html><meta charset="utf-8"><title>Automation ${name}</title>
<input id="draft" value="draft-${name}"><button id="click" onclick="window.clicks++;window.trusted=event.isTrusted">Click</button>
<script>
window.pageName=${JSON.stringify(name)};window.marker=crypto.randomUUID();window.clicks=0;window.events=[];
sessionStorage.marker ||= crypto.randomUUID();localStorage['automation-'+window.pageName]='stored';
document.cookie='automation'+window.pageName+'=cookie; Path=/; SameSite=Lax';
for(const type of ['mousedown','mouseup','click','keydown','keyup'])addEventListener(type,e=>window.events.push({type,button:e.button,buttons:e.buttons,key:e.key,trusted:e.isTrusted}),true);
window.readState=()=>({name:pageName,marker,session:sessionStorage.marker,draft:document.querySelector('#draft').value,local:localStorage['automation-'+pageName],history:history.length,cookie:document.cookie.split("; ").find(c=>c.startsWith("automation"+pageName+"="))});
</script>`, { headers: { 'content-type': 'text/html', 'cache-control': 'no-store' } });
}});
const child = spawn(resolve(hostPath), ['--native-host'], { env: { ...process.env, EXPLORERKIT_MINIDRIVER: resolve(helperPath) }, stdio: ['pipe', 'pipe', 'pipe'] });
const events: any[] = [];
const waiters = new Set<() => void>();
let exited = false;
let exitCode: number | null = null;
let exitSignal: string | null = null;
createInterface({ input: child.stdout }).on('line', line => {
  try { const event = JSON.parse(line); events.push(event); record('host', event); }
  catch { record('hostText', line); }
  for (const wake of waiters) wake();
});
child.stderr.on('data', data => record('stderr', data.toString()));
child.on('exit', (code, signal) => { exited = true; exitCode = code; exitSignal = signal; record('exit', { code, signal }); for (const wake of waiters) wake(); });
child.on('error', error => { record('spawnError', String(error)); exited = true; for (const wake of waiters) wake(); });
function waitHost(predicate: (event: any) => boolean, after = 0, timeout = 20_000): Promise<any> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => { waiters.delete(wake); reject(new Error('Host event timeout')); }, timeout);
    const wake = () => {
      const event = events.slice(after).find(predicate);
      if (event) { clearTimeout(timer); waiters.delete(wake); resolve(event); }
      else if (exited) { clearTimeout(timer); waiters.delete(wake); reject(new Error(`Host exited: ${exitCode}/${exitSignal}`)); }
    };
    waiters.add(wake); wake();
  });
}
async function host(command: string, fields: Record<string, unknown> = {}, allowError = false) {
  const id = crypto.randomUUID(), after = events.length;
  child.stdin.write(JSON.stringify({ id, command, ...fields }) + '\n');
  const result = await waitHost(event => event.id === id, after);
  if (!result.ok && result.event !== 'done' && !allowError) throw new Error(`Host ${command}: ${result.error}`);
  return result;
}
const value = async (command: string, fields: Record<string, unknown> = {}) => (await host(command, fields)).value;
const settle = <T>(promise: Promise<T>) => promise.then(value => ({ ok: true as const, value }), error => ({ ok: false as const, error: String(error) }));
async function until<T>(read: () => Promise<T>, predicate: (value: T) => boolean, label: string, timeout = 15_000): Promise<T> {
  const deadline = Date.now() + timeout;
  let latest: T | undefined;
  while (Date.now() < deadline) {
    latest = await read();
    if (predicate(latest)) return latest;
    await Bun.sleep(30);
  }
  throw new Error(`${label} timed out: ${JSON.stringify(latest)}`);
}
let driverUrl = '', session = '';
async function http(method: string, path: string, body?: unknown, allowError = false) {
  const response = await fetch(driverUrl + path, { method, headers: { 'content-type': 'application/json' }, body: body === undefined ? undefined : JSON.stringify(body), signal: AbortSignal.timeout(20_000) });
  const result: any = await response.json();
  record('http', { method, path, body, status: response.status, result });
  if ((!response.ok || result.value?.error) && !allowError) throw new Error(`${method} ${path}: ${JSON.stringify(result)}`);
  return { status: response.status, value: result.value };
}
const command = async (method: string, path: string, body?: unknown) => (await http(method, `/session/${session}${path}`, body)).value;
const script = (source: string, args: unknown[] = []) => command('POST', '/execute/sync', { script: source, args });
class Bidi {
  socket: WebSocket;
  messages: any[] = [];
  next = 0;
  pending = new Map<number, { resolve: (value: any) => void; reject: (error: Error) => void; timer: ReturnType<typeof setTimeout> }>();
  constructor(url: string) {
    this.socket = new WebSocket(url);
    this.socket.addEventListener('message', event => {
      const message = JSON.parse(String(event.data)); this.messages.push(message); record('bidi', message);
      const pending = this.pending.get(message.id);
      if (!pending) return;
      clearTimeout(pending.timer); this.pending.delete(message.id);
      if (message.type === 'error') pending.reject(new Error(JSON.stringify(message))); else pending.resolve(message.result);
    });
    this.socket.addEventListener('close', () => {
      for (const pending of this.pending.values()) { clearTimeout(pending.timer); pending.reject(new Error('BiDi connection closed')); }
      this.pending.clear();
    });
  }
  async open() {
    await new Promise<void>((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error('BiDi open timeout')), 10_000);
      this.socket.addEventListener('open', () => { clearTimeout(timer); resolve(); }, { once: true });
      this.socket.addEventListener('error', () => { clearTimeout(timer); reject(new Error('BiDi open error')); }, { once: true });
    });
  }
  send(method: string, params: unknown): Promise<any> {
    const id = ++this.next; record('bidiSend', { id, method, params });
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => { this.pending.delete(id); reject(new Error(`BiDi timeout: ${method}`)); }, 20_000);
      this.pending.set(id, { resolve, reject, timer }); this.socket.send(JSON.stringify({ id, method, params }));
    });
  }
  close() { this.socket.close(); }
}
let bidi: Bidi | undefined;
const sockets: Bidi[] = [];
const drivers = new Set<number>();
async function connect() {
  const connection = await value('connect'); driverUrl = connection.driverUrl; drivers.add(connection.driverPid);
  await until(async () => { try { return await http('GET', '/status'); } catch { return null; } }, x => x !== null, 'helper listener');
  const created = (await http('POST', '/session', { capabilities: { alwaysMatch: { webSocketUrl: true, unhandledPromptBehavior: 'ignore' } } })).value;
  session = created.sessionId;
  check(typeof session === 'string', 'standard session created');
  bidi = new Bidi(created.capabilities.webSocketUrl); sockets.push(bidi); await bidi.open();
  await bidi.send('session.subscribe', { events: ['browsingContext.contextCreated', 'browsingContext.contextDestroyed', 'browsingContext.load'] });
  const snapshot = await value('snapshot');
  check(snapshot.status.state === 'active' && snapshot.status.sessionId === session, 'public status matches native paired session', snapshot.status);
}
async function context(name: string) {
  const snapshot = await value('snapshot');
  const view = snapshot.views.find((view: any) => view.name === name);
  check(view?.context?.sessionId === session, `session-local mapping for ${name}`, view);
  return view.context.handle as string;
}
async function select(name: string) { const handle = await context(name); await command('POST', '/window', { handle }); return handle; }
async function nativeSet(names: string[], label: string) {
  const expected = (await Promise.all(names.map(context))).sort();
  const handles = (await command('GET', '/window/handles') as string[]).sort();
  const tree = await bidi!.send('browsingContext.getTree', {});
  const treeHandles = tree.contexts.map((item: any) => item.context).sort();
  check(JSON.stringify(handles) === JSON.stringify(expected), `${label}: native handles contain exactly live eligible views`, { expected, handles });
  check(JSON.stringify(treeHandles) === JSON.stringify(expected), `${label}: BiDi tree contains no ghost context`, { expected, treeHandles });
}
async function create(name: string, fields: Record<string, unknown> = {}) { return value('create', { name, ...fields }); }
async function pageReady(name: string) { await select(name); await until(() => script('return typeof window.readState'), x => x === 'function', `${name} load`); }
const canonical = (input: any): any => input && typeof input === 'object' ? (Array.isArray(input) ? input.map(canonical) : Object.fromEntries(Object.keys(input).sort().map(key => [key, canonical(input[key])]))) : input;
const original = new Map<string, any>();
const originalViews = new Map<string, any>();
async function preserve(label: string) {
  const snapshot = await value('snapshot');
  for (const name of ['A', 'B']) {
    const view = snapshot.views.find((view: any) => view.name === name);
    const before = originalViews.get(name);
    check(view?.sdk === before.sdk && view?.nativeIdentity?.native === before.nativeIdentity.native && view?.nativeIdentity?.pool === before.nativeIdentity.pool && view?.nativeIdentity?.store === before.nativeIdentity.store, `${label}: original ${name} SDK/native identity survives`, { before, view });
    await select(name);
    const current = await script('return readState()');
    check(JSON.stringify(canonical(current)) === JSON.stringify(canonical(original.get(name))), `${label}: standard script reads preserved ${name} document and state`, current);
  }
}
async function startCreate(kind: 'classic' | 'bidi', params: Record<string, unknown> = { type: 'tab' }) {
  const after = events.length;
  const result = settle(kind === 'classic' ? command('POST', '/window/new', params) : bidi!.send('browsingContext.create', params));
  const request = await waitHost(event => event.event === 'newViewRequest', after);
  return { request, result };
}
async function triggerDialog(name: string, kind: 'alert' | 'confirm' | 'prompt', message: string) {
  const after = events.length;
  const source = `setTimeout(() => {window.answer=${kind}(${JSON.stringify(message)}${kind === 'prompt' ? ',"default"' : ''})}, 0); 0`;
  await value('eval', { name, evaluationId: crypto.randomUUID(), script: source });
  return waitHost(event => event.event === 'sdkEvent' && event.dialog?.message === message, after);
}
async function proveRetirementBarrier() {
  for (const disconnectAfter of [false, true]) {
    const label = disconnectAfter ? 'batch disconnect' : 'batch retry';
    const rejectedRequest = await startCreate('bidi', { type: 'tab', userContext: 'does-not-exist' });
    const retryRequest = await startCreate('classic');
    const rejectedName = `rejected-${disconnectAfter}`, retryName = `retry-${disconnectAfter}`;
    const rejectedView = await create(rejectedName);
    await create(retryName);
    const beforeBatch = await value('snapshot');
    const retryBefore = beforeBatch.views.find((view: any) => view.name === retryName);
    const batch = await value('resolveBatch', { resolutions: [
      { token: rejectedRequest.request.token, view: rejectedName },
      { token: retryRequest.request.token, view: retryName },
    ], disconnectAfter });
    check(batch.results[0].ok === true && batch.results[1].ok === false && /retir/i.test(batch.results[1].error), `${label}: back-to-back resolution stops before second adoption`, batch);
    check(batch.disconnected === disconnectAfter, `${label}: requested connection state is applied in the same host turn`);
    const rejectedResult = await rejectedRequest.result;
    check(!rejectedResult.ok && (disconnectAfter || /no such user context/.test(rejectedResult.error)), `${label}: first candidate receives native rejection or connection closure`, rejectedResult);
    const afterBatch = await until(() => value('snapshot'), snapshot => !snapshot.views.some((view: any) => view.name === rejectedName), `${label}: ordinary update retires first owner`);
    const retryAfter = afterBatch.views.find((view: any) => view.name === retryName);
    check(retryAfter?.sdk === retryBefore.sdk && retryAfter?.nativeIdentity?.native === retryBefore.nativeIdentity.native, `${label}: pre-adoption error preserves the exact second candidate`, { retryBefore, retryAfter });
    await waitHost(event => event.closed === true && event.sdk === rejectedView.sdk);
    if (disconnectAfter) {
      const pendingResult = await retryRequest.result;
      check(!pendingResult.ok && /Session terminated without a reply/.test(pendingResult.error), 'batch disconnect: pending second command fails without timeout', pendingResult);
      const stale = await host('resolve', { token: retryRequest.request.token, view: retryName }, true);
      check(stale.ok === false && /stale/.test(stale.error), 'batch disconnect: second token is stale before any adoption', stale);
      await connect();
      const stillStale = await host('resolve', { token: retryRequest.request.token, view: retryName }, true);
      check(stillStale.ok === false && /stale/.test(stillStale.error), 'batch disconnect: reconnect does not revive the second token', stillStale);
    } else {
      await value('resolve', { token: retryRequest.request.token, view: retryName });
      const retried = await retryRequest.result;
      check(retried.ok && retried.value.handle === await context(retryName), 'batch retry: exact second token and candidate succeed after ordinary update', retried);
      if (retried.ok) await until(async () => bidi!.messages.find(message => message.method === 'browsingContext.load' && message.params.context === retried.value.handle && message.params.url === 'about:blank'), Boolean, 'batch retry: initial native document load');
    }
    await nativeSet(['A', 'B', retryName], `${label}: native owner settlement`);
    await preserve(label);
    check(events.filter(event => event.closed === true && event.sdk === rejectedView.sdk).length === 1, `${label}: rejected candidate emits exactly one SDK close receipt`);
    await value('destroy', { name: retryName });
    await nativeSet(['A', 'B'], `${label}: cleanup`);
  }
}
const retirementOnly = process.env.EXPLORERKIT_RETIREMENT_PROOF === '1';
let failure = '';
try {
  await waitHost(event => event.event === 'ready');
  check(!(await host('connect', {}, true)).ok, 'zero eligible views reject connection without a helper');
  await create('A', { url: fixture.url.origin + '/A' });
  await create('B', { url: fixture.url.origin + '/B', eligible: false });
  await create('U', { attach: false, eligible: false });
  await value('enroll', { name: 'U', enabled: true });
  const before = await value('snapshot');
  check(before.views.find((v: any) => v.name === 'U').context === null, 'unattached logical view accepts eligibility without native context');
  await connect();
  await nativeSet(['A'], 'initial enrollment');
  check(!(await host('connect', {}, true)).ok, 'second connection rejected while original remains active');
  await nativeSet(['A'], 'rejected second connection');
  await value('enroll', { name: 'B', enabled: true });
  await nativeSet(['A', 'B'], 'late enrollment of human-opened view');
  const navigationContext = await select('A');
  const beforeNavigation = bidi!.messages.length;
  await command('POST', '/url', { url: fixture.url.origin + '/A' });
  const loaded = await until(async () => bidi!.messages.slice(beforeNavigation).find(message => message.method === 'browsingContext.load' && message.params.context === navigationContext), Boolean, 'native BiDi load event');
  check(loaded.params.url === fixture.url.origin + '/A', 'standard navigation delivers the native BiDi load event for the selected view');
  for (const name of ['A', 'B']) { await pageReady(name); original.set(name, await script('return readState()')); }
  const originalSnapshot = await value('snapshot');
  for (const name of ['A', 'B']) originalViews.set(name, originalSnapshot.views.find((view: any) => view.name === name));
  if (retirementOnly) {
    await proveRetirementBarrier();
  } else {
    check(!(await host('enroll', { name: 'A', enabled: false }, true)).ok, 'connected eligibility is not silently revoked');
    await value('detach', { name: 'A' });
    await nativeSet(['A', 'B'], 'ordinary detach'); await preserve('ordinary detach');
    await value('attach', { name: 'A' });
    await nativeSet(['A', 'B'], 'ordinary reattach'); await preserve('ordinary reattach');
    await value('foreground'); await select('A');
    const button = await command('POST', '/element', { using: 'css selector', value: '#click' });
    const element = button['element-6066-11e4-a52e-4f735466cecf'];
    await command('POST', `/element/${element}/click`, {});
    check(await script('return clicks===1 && trusted===true'), 'standard native element click is trusted and delivered once');
    const rect = await command('GET', `/element/${element}/rect`);
    await command('POST', '/actions', { actions: [{ type: 'pointer', id: 'mouse', parameters: { pointerType: 'mouse' }, actions: [{ type: 'pointerMove', x: Math.round(rect.x + rect.width / 2), y: Math.round(rect.y + rect.height / 2), origin: 'viewport' }, { type: 'pointerDown', button: 0 }] }] });
    const ups = await script('return events.filter(e=>e.type==="mouseup").length');
    await command('DELETE', '/actions');
    check(await script('return events.filter(e=>e.type==="mouseup").length') === ups + 1, 'release actions releases a successfully held native mouse button');
    check((await command('GET', '/screenshot')).startsWith('iVBORw0KGgo'), 'native screenshot command returns a PNG');
    // A default/no-hook run must work. An explicit hook is verified by a second
    // invocation with EXPLORERKIT_SELECTION_HOOK=1 using this same client.
    if (process.env.EXPLORERKIT_SELECTION_HOOK) {
      await value('hideView', { name: 'B' });
      check((await value('snapshot')).views.find((v: any) => v.name === 'B').nativeIdentity.visible === false, 'host hides the native view before selection');
      await value('background');
      await until(() => value('snapshot'), snapshot => snapshot.appActive === false && snapshot.views.find((v: any) => v.name === 'A').nativeIdentity.visible === true, 'visible inactive host window');
      await select('B'); const snap = await value('snapshot');
      check(snap.selected === snap.views.find((v: any) => v.name === 'B').sdk, 'optional selection hook receives the actual SDK view');
      check(snap.views.find((v: any) => v.name === 'B').nativeIdentity.visible === true, 'optional selection hook synchronously reveals the hidden native view');
      check(snap.appActive === false, 'native selection does not force application activation');
      await value('foreground');
    }
    const rejected = await startCreate('classic');
    check(!(await host('resolve', { token: rejected.request.token, view: 'A' }, true)).ok, 'existing view cannot be adopted as a disposable candidate');
    await value('attach', { name: 'U' });
    check(!(await host('resolve', { token: rejected.request.token, view: 'U' }, true)).ok, 'old logical view attached after request still fails freshness');
    await value('resolve', { token: rejected.request.token, view: null });
    const declined = await rejected.result;
    check(!declined.ok && /failed to create a new browsing context/.test(declined.error), 'declined creation returns native error without nil completion', declined);
    await nativeSet(['A', 'B', 'U'], 'rejected existing candidates remain untouched');
    await value('destroy', { name: 'U' });
    for (const kind of ['classic', 'bidi'] as const) {
      const pending = await startCreate(kind);
      const candidate = await create(`failed-${kind}`, { failAttach: true });
      check(candidate.created === false && candidate.rollback === true, `${kind}: ordinary attach error rolls back partial SDK view`, candidate);
      await value('resolve', { token: pending.request.token, view: null });
      const result = await pending.result;
      check(!result.ok && /failed to create a new browsing context/.test(result.error), `${kind}: caller receives native creation error`, result);
      await nativeSet(['A', 'B'], `${kind} failed attach`); await preserve(`${kind} failed attach`);
    }
    for (const kind of ['classic', 'bidi'] as const) {
      const pending = await startCreate(kind);
      const name = `created-${kind}`;
      await create(name);
      if (kind === 'classic') {
        await value('detach', { name });
        check(!(await host('resolve', { token: pending.request.token, view: name }, true)).ok, 'fresh detached candidate rejected before adoption');
        await value('attach', { name });
      }
      await value('resolve', { token: pending.request.token, view: name });
      const result = await pending.result;
      check(result.ok, `${kind}: standard create succeeds through public resolution`, result);
      if (!result.ok) throw new Error(result.error);
      const handle = await context(name);
      check((result.value.handle ?? result.value.context) === handle, `${kind}: native result maps to the supplied SDK view`);
      await until(async () => bidi!.messages.find(message => message.method === 'browsingContext.load' && message.params.context === handle && message.params.url === 'about:blank'), Boolean, `${kind}: initial native document load`);
      await nativeSet(['A', 'B', name], `${kind} successful creation`);
      await command('POST', '/window', { handle });
      check(await script('return location.href') === 'about:blank', `${kind}: standard script executes in the new context`);
      await command('DELETE', '/window');
      await until(() => value('snapshot'), snapshot => !snapshot.views.some((v: any) => v.name === name), 'native close retires SDK identity');
      await nativeSet(['A', 'B'], `${kind} explicit close`);
    }
    const invalid = await startCreate('bidi', { type: 'tab', userContext: 'does-not-exist' });
    await create('invalid-candidate');
    await value('resolve', { token: invalid.request.token, view: 'invalid-candidate' });
    const invalidResult = await invalid.result;
    check(!invalidResult.ok && /no such user context/.test(invalidResult.error), 'native validation retains its own error after host preparation', invalidResult);
    await until(() => value('snapshot'), snapshot => !snapshot.views.some((v: any) => v.name === 'invalid-candidate'), 'unused candidate retirement');
    await nativeSet(['A', 'B'], 'native rejected candidate'); await preserve('native rejected candidate');
    observations.push({ invalidCreateEvents: bidi!.messages.filter(message => message.type === 'event') });
    await proveRetirementBarrier();
    // Two independent pending commands ensure preparation is request-specific.
    const first = await startCreate('classic'), second = await startCreate('bidi');
    await create('shared-candidate');
    await value('resolve', { token: second.request.token, view: 'shared-candidate' });
    check((await second.result).ok, 'overlapping request can resolve independently');
    check(!(await host('resolve', { token: first.request.token, view: 'shared-candidate' }, true)).ok, 'already claimed candidate cannot fulfill a second request');
    await value('resolve', { token: first.request.token, view: null });
    check(!(await first.result).ok, 'other overlapping request receives its own failure');
    await value('destroy', { name: 'shared-candidate' }); await nativeSet(['A', 'B'], 'overlapping preparation settled');
    await select('A');
    const popupAfter = events.length;
    await value('enablePopups', { name: 'A' });
    await script('window.open(arguments[0], "_blank"); return true', [fixture.url.origin + '/popup']);
    const popup = await waitHost(event => typeof event.popupName === 'string', popupAfter);
    check(typeof popup.createdSdk === 'number', 'native popup has an ordinary registered SDK handle', popup);
    await nativeSet(['A', 'B', popup.popupName], 'popup inherits source automation eligibility');
    await select(popup.popupName);
    await until(() => script('return document.title'), title => title === 'Automation popup', 'popup navigation');
    await command('DELETE', '/window');
    await until(() => value('snapshot'), snapshot => !snapshot.views.some((v: any) => v.name === popup.popupName), 'native popup close');
    await nativeSet(['A', 'B'], 'native popup retirement'); await preserve('native popup retirement');
    for (const kind of ['classic', 'bidi'] as const) {
      const pending = await startCreate(kind);
      await value('disconnect');
      const result = await pending.result;
      check(!result.ok && /Session terminated without a reply/.test(result.error), `${kind}: pending disconnect fails caller without timeout`, result);
      await create(`late-${kind}`);
      check(!(await host('resolve', { token: pending.request.token, view: `late-${kind}` }, true)).ok, `${kind}: stale result rejected before adoption`);
      const snap = await value('snapshot');
      check(snap.views.some((v: any) => v.name === `late-${kind}`), `${kind}: stale result leaves supplied view with host`);
      await value('destroy', { name: `late-${kind}` });
      await connect(); await nativeSet(['A', 'B'], `${kind} reconnect`); await preserve(`${kind} reconnect`);
    }
    await select('A');
    const prompt = await triggerDialog('A', 'prompt', 'driver prompt');
    check(await command('GET', '/alert/text') === 'driver prompt', 'native dialog query reads the shared Rust pending record');
    await command('POST', '/alert/text', { text: 'driver answer' });
    await command('POST', '/alert/accept', {});
    await waitHost(event => event.dismissed === prompt.dialog.id);
    check(await script('return answer') === 'driver answer', 'driver answer completes the captured native prompt');
    check(!(await host('dialog', { name: 'A', dialogId: prompt.dialog.id, confirmed: true, value: 'second answer' }, true)).ok, 'host cannot resolve a driver-consumed dialog twice');
    const sharedPrompt = await triggerDialog('A', 'prompt', 'shared prompt text');
    await command('POST', '/alert/text', { text: 'driver text accepted by host' });
    await value('dialog', { name: 'A', dialogId: sharedPrompt.dialog.id, confirmed: true });
    await waitHost(event => event.dismissed === sharedPrompt.dialog.id);
    check(await script('return answer') === 'driver text accepted by host', 'host acceptance without a value uses the current shared prompt text');
    const confirm = await triggerDialog('A', 'confirm', 'host confirm');
    check(!(await host('dialog', { name: 'A', dialogId: confirm.dialog.id, confirmed: true, value: 'invalid' }, true)).ok, 'invalid host dialog answer does not consume pending state');
    check(await command('GET', '/alert/text') === 'host confirm', 'invalid answer leaves the same native prompt pending');
    await value('dialog', { name: 'A', dialogId: confirm.dialog.id, confirmed: false });
    check(await script('return answer') === false, 'host answer completes the same native dialog');
    check((await http('POST', `/session/${session}/alert/accept`, {}, true)).value.error === 'no such alert', 'driver cannot resolve a host-consumed dialog twice');
    const preservedPrompt = await triggerDialog('A', 'confirm', 'disconnect preserves dialog');
    await value('disconnect');
    check(!(await host('enroll', { name: 'A', enabled: false }, true)).ok, 'pending captured dialog prevents opt-out while disconnected');
    await value('dialog', { name: 'A', dialogId: preservedPrompt.dialog.id, confirmed: false });
    await connect(); await select('A');
    check(await script('return answer') === false, 'disconnect preserves dialog for host resolution');
    await create('dialog-close', { url: fixture.url.origin + '/dialog-close' });
    const closingContext = await context('dialog-close');
    await until(async () => bidi!.messages.find(message => message.method === 'browsingContext.load' && message.params.context === closingContext && message.params.url === fixture.url.origin + '/dialog-close'), Boolean, 'dialog fixture native load');
    await pageReady('dialog-close');
    const closing = await triggerDialog('dialog-close', 'prompt', 'close with pending prompt');
    await value('destroy', { name: 'dialog-close' });
    await waitHost(event => event.dismissed === closing.dialog.id);
    await nativeSet(['A', 'B'], 'pending-dialog view destroy');
    check(events.filter(event => event.dismissed === closing.dialog.id).length === 1, 'explicit destroy preserves one terminal dialog dismissal receipt');
    check(!(await host('dialog', { name: 'dialog-close', dialogId: closing.dialog.id, confirmed: true }, true)).ok, 'retired dialog rejects late host answer');
    await select('A');
    bidi!.close(); await Bun.sleep(50);
    check(await command('GET', '/title') === 'Automation A', 'BiDi-only disconnect preserves Classic session');
    await command('DELETE', '');
    await until(() => value('snapshot'), snapshot => snapshot.status.state !== 'active', 'Delete Session cleanup');
    await connect(); await nativeSet(['A', 'B'], 'Delete Session reconnect'); await preserve('Delete Session reconnect');
    await value('crashDriver');
    await until(() => value('snapshot'), snapshot => snapshot.status.state === 'failed', 'helper death observed');
    await connect(); await nativeSet(['A', 'B'], 'helper crash reconnect'); await preserve('helper crash reconnect');
  }
  const down = await startCreate('classic');
  const shutdown = process.env.EXPLORERKIT_AUTOMATION_TEARDOWN === 'shutdown';
  const teardown = shutdown ? 'explicit shutdown' : 'drop';
  await triggerDialog('A', 'prompt', `Runtime ${teardown} with pending prompt and creation`);
  const dropped = await value(shutdown ? 'shutdownRuntime' : 'dropRuntime');
  check(typeof dropped.weakError === 'string', `${teardown}: weak control reports host destruction`);
  check(!(await down.result).ok, `${teardown}: Runtime destruction ends a queued creation call`);
  check(!(await host('snapshot', {}, true)).value?.status?.state, 'dropped control has no live status');
  await host('quit');
  await until(async () => exited, x => x, 'normal host exit');
  check(exitCode === 0 && exitSignal === null, 'host exits normally after lifetime checks', { exitCode, exitSignal });
  const reapEvents = events.filter(event => event.event === 'helperReaped');
  check(reapEvents.every(event => String(event.result).startsWith('Ok(')), 'every helper wait succeeds', reapEvents);
  const reaped = new Set(reapEvents.map(event => event.pid));
  check([...drivers].every(pid => reaped.has(pid)), 'host reaps every helper it launched', { drivers: [...drivers], reaped: [...reaped] });
} catch (error) {
  failure = error instanceof Error ? error.stack ?? String(error) : String(error);
  console.error(failure);
} finally {
  for (const socket of sockets) socket.close();
  if (!exited) {
    await host('quit').catch(error => record('cleanupError', String(error)));
    await until(async () => exited, x => x, 'cleanup exit', 5_000).catch(() => child.kill('SIGTERM'));
  }
  fixture.stop(true);
  const output = process.env.EXPLORERKIT_AUTOMATION_EVIDENCE;
  if (output) {
    await mkdir(output, { recursive: true });
    await writeFile(resolve(output, 'client-result.json'), JSON.stringify({ mode: retirementOnly ? 'retirement' : 'full', passed: !failure, failure, checks, observations, hostPid: child.pid, driverPids: [...drivers], exitCode, exitSignal }, null, 2));
    await writeFile(resolve(output, 'client-transcript.json'), JSON.stringify(records, null, 2));
  }
}
if (failure) process.exitCode = 1;
