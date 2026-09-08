import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import test from 'node:test';
import vm from 'node:vm';

const userscriptPath = resolve(import.meta.dirname, '../dist/ai-chat-memory.user.js');

function loadTestApi(overrides = {}) {
    const values = new Map();
    const context = {
        URL,
        TextDecoder,
        TextEncoder,
        __AI_CHAT_MEMORY_TEST_MODE__: true,
        console: { log() {}, warn() {}, error() {} },
        // 油猴运行时具备标准浏览器定时器；userscript 节流写依赖 setTimeout/clearTimeout，此处提供等价物。
        setTimeout: (fn, ms, ...args) => setTimeout(fn, ms, ...args),
        clearTimeout: id => clearTimeout(id),
        fetch: async () => new Response('{}', { headers: { 'content-type': 'application/json' } }),
        location: { hostname: 'example.invalid', pathname: '/' },
        GM_getValue: (key, fallback) => values.has(key) ? values.get(key) : fallback,
        GM_setValue: (key, value) => values.set(key, value),
        GM_deleteValue: key => values.delete(key),
        ...overrides
    };
    context.globalThis = context;
    vm.runInNewContext(readFileSync(userscriptPath, 'utf8'), context, { filename: userscriptPath });
    assert.ok(context.__AI_CHAT_MEMORY_TEST_API__, 'userscript should expose its module API in test mode');
    return { api: context.__AI_CHAT_MEMORY_TEST_API__, context, values };
}

function plain(value) {
    return JSON.parse(JSON.stringify(value));
}

test('metadata block declares @connect so GM_xmlhttpRequest survives strict managers', () => {
    const head = readFileSync(userscriptPath, 'utf8').split(/\r?\n/).slice(0, 50).join('\n');
    assert.match(head, /^\/\/\s*@connect\s+\*\s*$/m, 'the metadata block must whitelist cross-origin download hosts via @connect *');
    assert.match(head, /^\/\/\s*@grant\s+GM_xmlhttpRequest\s*$/m, 'the ZIP download relies on GM_xmlhttpRequest');
});

test('redacts credentials while preserving DeepSeek protocol metadata', () => {
    const { api } = loadTestApi();
    const redacted = api.CaptureRedactor.redactExchange({
        request: {
            url: 'https://chat.deepseek.com/api/v0/chat/history_messages?chat_session_id=session-1&did=device-1&token=token-1',
            headers: {
                authorization: 'Bearer secret-token',
                Cookie: 'sid=secret',
                'x-ds-pow-response': 'pow-secret',
                'x-client-version': '2.2.0',
                'x-client-platform': 'web'
            },
            body: { prompt: 'keep me', secret: 'remove me' }
        },
        response: {
            headers: { 'set-cookie': 'sid=secret', 'content-type': 'application/json' },
            body: { authorization: 'Bearer nested', content: 'keep response' }
        }
    });

    const url = new URL(redacted.request.url);
    assert.equal(url.searchParams.get('chat_session_id'), 'session-1');
    assert.equal(url.searchParams.get('did'), '{REDACTED}');
    assert.equal(url.searchParams.get('token'), '{REDACTED}');
    assert.deepEqual(plain(redacted.request.headers), {
        'x-client-version': '2.2.0',
        'x-client-platform': 'web'
    });
    assert.equal(redacted.request.body.prompt, 'keep me');
    assert.equal(redacted.request.body.secret, '{REDACTED}');
    assert.deepEqual(plain(redacted.response.headers), { 'content-type': 'application/json' });
    assert.equal(redacted.response.body.authorization, '{REDACTED}');
    assert.equal(redacted.response.body.content, 'keep response');
});

test('parses SSE event order, multiline data, unknown events, and invalid JSON', () => {
    const { api } = loadTestApi();
    const events = api.SseParser.parse([
        'event: ready',
        'data: {"model_type":"vision"}',
        '',
        'data: {"p":"response/fragments/-1/content",',
        'data: "o":"APPEND","v":"ok"}',
        '',
        'event: future_event',
        'data: not-json',
        ''
    ].join('\n'), () => '2026-07-27T00:00:00.000Z');

    assert.equal(events.length, 3);
    assert.deepEqual(plain(events.map(event => event.event)), ['ready', 'message', 'future_event']);
    assert.equal(events[0].data_json.model_type, 'vision');
    assert.equal(events[1].data_json.o, 'APPEND');
    assert.equal(events[1].data_json.v, 'ok');
    assert.equal(events[2].data_raw, 'not-json');
    assert.equal(events[2].data_json, null);
    assert.equal(events[2].captured_at, '2026-07-27T00:00:00.000Z');
});

test('classifies DeepSeek API exchanges and resolves their session id', () => {
    const { api } = loadTestApi();
    const classifier = api.DeepSeekExchangeClassifier;

    assert.equal(classifier.classify('https://chat.deepseek.com/api/v0/client/settings?scope=model'), 'model_settings');
    assert.equal(classifier.classify('https://chat.deepseek.com/api/v0/chat/history_messages?chat_session_id=query-id'), 'history');
    assert.equal(classifier.classify('https://chat.deepseek.com/api/v0/chat/completion'), 'completion');
    assert.equal(classifier.classify('https://chat.deepseek.com/api/v0/file/upload_file'), 'file_upload');
    assert.equal(classifier.classify('https://chat.deepseek.com/api/v0/chat_session/fetch_page'), 'session_page');

    assert.equal(classifier.sessionId({ request: { body: { chat_session_id: 'body-id' } } }), 'body-id');
    assert.equal(classifier.sessionId({ request: { url: 'https://chat.deepseek.com/api/v0/chat/history_messages?chat_session_id=query-id' } }), 'query-id');
    assert.equal(classifier.sessionId({ response: { body: { data: { biz_data: { chat_session: { id: 'response-id' } } } } } }), 'response-id');
    assert.equal(classifier.sessionId({ response: { sse_events: [{ event: 'ready', data_json: { chat_session_id: 'sse-id' } }] } }), 'sse-id');
    assert.equal(classifier.sessionId({}, '/a/chat/s/path-id'), 'path-id');
});

test('stores latest snapshots and bounded per-session exchange logs', async () => {
    const { api, values } = loadTestApi();
    let tick = 0;
    const store = new api.CaptureStore({
        getValue: (key, fallback) => values.has(key) ? values.get(key) : fallback,
        setValue: (key, value) => values.set(key, value),
        now: () => `2026-07-27T00:00:${String(tick++).padStart(2, '0')}.000Z`,
        config: { maxCompletionExchanges: 2, maxFileExchanges: 2, maxOtherExchanges: 2 }
    });

    await store.record({ kind: 'model_settings', request: { url: 'https://chat.deepseek.com/api/v0/client/settings?scope=model' }, response: { body: { version: 1 } } });
    await store.record({ kind: 'model_settings', request: { url: 'https://chat.deepseek.com/api/v0/client/settings?scope=model' }, response: { body: { version: 2 } } });
    for (let id = 1; id <= 3; id++) {
        await store.record({ id: `completion-${id}`, kind: 'completion', sessionId: 'session-1', request: { url: 'https://chat.deepseek.com/api/v0/chat/completion' }, response: { body: { id } } });
    }
    await store.record({ id: 'history-1', kind: 'history', sessionId: 'session-1', response: { body: { version: 1 } } });
    await store.record({ id: 'history-2', kind: 'history', sessionId: 'session-1', response: { body: { version: 2 } } });

    const exported = plain(store.exportSession('session-1'));
    assert.equal(exported.schema_version, 1);
    assert.equal(exported.client.model_settings.response.body.version, 2);
    assert.deepEqual(exported.session.completion_exchanges.map(item => item.id), ['completion-2', 'completion-3']);
    assert.equal(exported.session.latest_native_history.id, 'history-2');
    assert.equal(exported.session.updated_at, '2026-07-27T00:00:06.000Z');
});

test('truncates oversized response bodies without losing their field structure', async () => {
    const { api, values } = loadTestApi();
    const store = new api.CaptureStore({
        getValue: (key, fallback) => values.has(key) ? values.get(key) : fallback,
        setValue: (key, value) => values.set(key, value),
        config: { maxResponseBytes: 64 }
    });

    await store.record({
        id: 'large',
        kind: 'completion',
        sessionId: 'session-1',
        response: { body: { content: 'x'.repeat(1000), nested: { keepShape: true } } }
    });

    const response = plain(store.exportSession('session-1').session.completion_exchanges[0].response);
    assert.equal(response.truncated, true);
    assert.ok(response.byte_length > 1000);
    assert.deepEqual(response.body_shape, { content: 'string', nested: { keepShape: 'boolean' } });
    assert.equal('body' in response, false);
});

test('preserves native and compatibility history snapshots separately', async () => {
    const { api, values } = loadTestApi();
    const store = new api.CaptureStore({
        getValue: (key, fallback) => values.has(key) ? values.get(key) : fallback,
        setValue: (key, value) => values.set(key, value)
    });

    await store.record({
        id: 'native-history',
        kind: 'history',
        sessionId: 'session-1',
        response: { body: { data: { biz_data: { chat_messages: [{ fragments: [{ content: 'native' }] }] } } } }
    });
    await store.record({
        id: 'compat-history',
        kind: 'history',
        sessionId: 'session-1',
        response: { body: { data: { biz_data: { chat_messages: [{ content: 'compatibility' }] } } } }
    });

    const session = plain(store.exportSession('session-1').session);
    assert.equal(session.latest_native_history.id, 'native-history');
    assert.equal(session.latest_compatibility_history.id, 'compat-history');
});

test('moves unassigned page exchanges into a session once the current path identifies it', async () => {
    const { api, values } = loadTestApi();
    const store = new api.CaptureStore({
        getValue: (key, fallback) => values.has(key) ? values.get(key) : fallback,
        setValue: (key, value) => values.set(key, value)
    });

    await store.record({
        id: 'unassigned-completion',
        kind: 'completion',
        request: { url: 'https://chat.deepseek.com/api/v0/chat/completion', body: { model_type: 'expert' } },
        response: { body: { content: 'done' } }
    });
    await store.record({
        id: 'history',
        kind: 'history',
        request: { url: 'https://chat.deepseek.com/api/v0/chat/history_messages?chat_session_id=session-1' },
        response: { body: { data: { biz_data: { chat_messages: [] } } } }
    }, '/a/chat/s/session-1');

    // record() 的磁盘写已改为节流，读持久化态前需 flush 落盘挂起的写。
    await store.flush();
    const persisted = plain(values.get(api.RuntimeConfig.captureStorageKey));
    assert.equal(persisted.unassigned.length, 0);
    assert.deepEqual(persisted.sessions['session-1'].completion_exchanges.map(item => item.id), ['unassigned-completion']);
});

test('evicts other exchanges before completions when a session exceeds its byte budget', async () => {
    const { api, values } = loadTestApi();
    const store = new api.CaptureStore({
        getValue: (key, fallback) => values.has(key) ? values.get(key) : fallback,
        setValue: (key, value) => values.set(key, value),
        config: { maxSessionBytes: 850, maxResponseBytes: 4096 }
    });

    await store.record({ id: 'completion-1', kind: 'completion', sessionId: 'session-1', response: { body: { content: 'c'.repeat(180) } } });
    await store.record({ id: 'other-1', kind: 'session_page', sessionId: 'session-1', response: { body: { content: 'o'.repeat(180) } } });
    await store.record({ id: 'completion-2', kind: 'completion', sessionId: 'session-1', response: { body: { content: 'n'.repeat(180) } } });

    const session = plain(store.exportSession('session-1').session);
    assert.deepEqual(session.other_exchanges, []);
    assert.ok(session.completion_exchanges.some(item => item.id === 'completion-2'));
    assert.ok(new TextEncoder().encode(JSON.stringify(session)).length <= 850);
});

test('captures fetch JSON and SSE without replacing or delaying the original response', async () => {
    const { api } = loadTestApi();
    const records = [];
    const jsonResponse = new Response(JSON.stringify({ data: { biz_data: { chat_session: { id: 'session-1' } } } }), {
        status: 200,
        headers: { 'content-type': 'application/json' }
    });
    const sseResponse = new Response('event: ready\ndata: {"model_type":"vision"}\n\ndata: {"v":{"response":{"fragments":[]}}}\n\n', {
        status: 200,
        headers: { 'content-type': 'text/event-stream' }
    });
    const responses = [jsonResponse, sseResponse];
    const window = { fetch: async () => responses.shift() };
    const capture = new api.NetworkCapture({
        window,
        store: { record: async exchange => records.push(plain(exchange)) },
        getPath: () => '/a/chat/s/session-1',
        now: () => '2026-07-27T00:00:00.000Z'
    });
    capture.install();

    const first = await window.fetch('https://chat.deepseek.com/api/v0/chat/history_messages?chat_session_id=session-1', {
        headers: { authorization: 'Bearer secret', 'x-client-version': '2.2.0' }
    });
    assert.equal(first, jsonResponse);
    const second = await window.fetch('https://chat.deepseek.com/api/v0/chat/completion', {
        method: 'POST',
        headers: { authorization: 'Bearer secret', 'content-type': 'application/json' },
        body: JSON.stringify({ chat_session_id: 'session-1', model_type: 'vision' })
    });
    assert.equal(second, sseResponse);
    await new Promise(resolve => setTimeout(resolve, 0));

    assert.equal(records.length, 2);
    assert.equal(records[0].kind, 'history');
    assert.equal(records[0].response.body.data.biz_data.chat_session.id, 'session-1');
    assert.equal(records[0].request.headers.authorization, undefined);
    assert.equal(records[0].request.headers['x-client-version'], '2.2.0');
    assert.equal(records[1].kind, 'completion');
    assert.equal(records[1].request.body.model_type, 'vision');
    assert.equal(records[1].response.format, 'sse');
    assert.equal(records[1].response.sse_events[0].data_json.model_type, 'vision');
    assert.deepEqual(records[1].response.sse_events[1].data_json.v.response.fragments, []);
});

test('captures XHR JSON while preserving the page-visible XHR result', async () => {
    const { api } = loadTestApi();
    const records = [];
    class FakeXHR {
        constructor() { this.listeners = new Map(); this.requestHeaders = {}; }
        open(method, url) { this.method = method; this.url = url; }
        setRequestHeader(name, value) { this.requestHeaders[name] = value; }
        addEventListener(name, listener) { this.listeners.set(name, listener); }
        getAllResponseHeaders() { return 'content-type: application/json\r\nx-client-version: 2.2.0\r\n'; }
        send(body) {
            this.sentBody = body;
            this.status = 200;
            this.responseText = JSON.stringify({ data: { biz_data: { chat_session: { id: 'session-xhr' } } } });
            this.listeners.get('loadend')?.call(this);
            return 'original-send-result';
        }
    }
    const window = { fetch: async () => new Response('{}'), XMLHttpRequest: FakeXHR };
    const capture = new api.NetworkCapture({
        window,
        store: { record: async exchange => records.push(plain(exchange)) },
        getPath: () => '/',
        now: () => '2026-07-27T00:00:00.000Z'
    });
    capture.install();

    const xhr = new window.XMLHttpRequest();
    xhr.open('GET', 'https://chat.deepseek.com/api/v0/chat/history_messages?chat_session_id=session-xhr');
    xhr.setRequestHeader('Authorization', 'Bearer secret');
    assert.equal(xhr.send(), 'original-send-result');
    await new Promise(resolve => setTimeout(resolve, 0));

    assert.equal(xhr.status, 200);
    assert.match(xhr.responseText, /session-xhr/);
    assert.equal(records.length, 1);
    assert.equal(records[0].source, 'xhr');
    assert.equal(records[0].session_id, 'session-xhr');
    assert.equal(records[0].request.headers.authorization, undefined);
    assert.equal(records[0].response.body.data.biz_data.chat_session.id, 'session-xhr');
});

test('serializes FormData as file metadata without copying binary content', () => {
    const { api } = loadTestApi();
    const body = new FormData();
    body.append('chat_session_id', 'session-file');
    body.append('file', new Blob(['private bytes'], { type: 'image/png' }), 'capture.png');

    const serialized = plain(api.NetworkCapture.serializeRequestBody(body));
    assert.equal(serialized.chat_session_id, 'session-file');
    assert.deepEqual(serialized.file, {
        kind: 'file',
        name: 'capture.png',
        type: 'image/png',
        size: 13
    });
    assert.equal(JSON.stringify(serialized).includes('private bytes'), false);
});

test('attaches references and session capture to a DeepSeek conversation payload', async () => {
    const { api } = loadTestApi();
    const conversation = { data: { biz_data: { chat_messages: [] } } };
    const captureStore = {
        flush: async () => {},
        exportSession: sessionId => ({ schema_version: 1, session: { id: sessionId } })
    };

    const result = await api.attachDeepSeekCapture(
        conversation,
        'session-1',
        captureStore,
        [{ cite_index: 2, url: 'https://example.com' }]
    );

    assert.equal(result, conversation);
    assert.deepEqual(plain(result._references), [{ cite_index: 2, url: 'https://example.com' }]);
    assert.equal(result._web_capture.schema_version, 1);
    assert.equal(result._web_capture.session.id, 'session-1');
});

test('exposes snake_case SSE fields and bounds oversized SSE payloads', async () => {
    const { api, values } = loadTestApi();
    const event = api.SseParser.parse('event: update_session\ndata: {"chat_session_id":"session-sse"}\n\n')[0];
    assert.equal(event.data_raw, '{"chat_session_id":"session-sse"}');
    assert.deepEqual(plain(event.data_json), { chat_session_id: 'session-sse' });
    const store = new api.CaptureStore({
        getValue: (key, fallback) => values.has(key) ? values.get(key) : fallback,
        setValue: (key, value) => values.set(key, value),
        config: { maxResponseBytes: 64 }
    });
    await store.record({
        id: 'large-sse', kind: 'completion', sessionId: 'session-sse',
        response: { format: 'sse', sse_events: Array.from({ length: 20 }, (_, index) => ({
            event: 'message', data_raw: 'x'.repeat(100 + index), data_json: { index }, captured_at: 'now'
        })) }
    });
    const response = plain(store.exportSession('session-sse').session.completion_exchanges[0].response);
    assert.equal(response.truncated, true);
    assert.equal('sse_events' in response, false);
    assert.equal(response.sse_events_summary.count, 20);
    assert.ok(response.sse_events_shape);
});

test('BridgeClient adds the client header and optional local secret through one request path', async () => {
    const { api } = loadTestApi();
    const values = new Map([['bridge_secret', 'local-secret']]);
    const calls = [];
    const client = new api.BridgeClient({
        getValue: (key, fallback) => values.has(key) ? values.get(key) : fallback,
        setValue: (key, value) => values.set(key, value),
        deleteValue: key => values.delete(key),
        fetch: async (url, options) => {
            calls.push({ url, options });
            return new Response('{}', { status: 200, headers: { 'content-type': 'application/json' } });
        }
    });
    const response = await client.request('/sessions/import', { headers: { 'Content-Type': 'application/json' } });
    assert.equal(response.status, 200);
    assert.equal(calls[0].url, 'http://localhost:19820/api/v1/sessions/import');
    assert.equal(calls[0].options.headers['X-AI-Chat-Memory-Client'], 'userscript-v1');
    assert.equal(calls[0].options.headers['X-AI-Chat-Memory-Secret'], 'local-secret');
    assert.equal(calls[0].options.headers['Content-Type'], 'application/json');
});

test('CredentialStore waits for a DeepSeek or Kimi token without exposing it to capture records', async () => {
    const { api } = loadTestApi();
    const values = new Map();
    const credentials = new api.CredentialStore({
        getValue: (key, fallback) => values.has(key) ? values.get(key) : fallback,
        setValue: (key, value) => values.set(key, value),
        deleteValue: key => values.delete(key),
        sleep: async () => {
            values.set('ds_token', 'TOKEN');
            values.set('ds_token_captured_at', Date.now());
        },
        waitTimeoutMs: 1000
    });
    assert.equal(await credentials.waitForToken('ds_token'), 'TOKEN');
    credentials.saveToken('kimi_token', 'KIMI', 'Kimi');
    assert.equal(credentials.getToken('kimi_token'), 'KIMI');
    credentials.clearToken('kimi_token');
    assert.equal(credentials.getToken('kimi_token'), null);
});

test('DeepSeek, Doubao, and Kimi adapters use injected list/detail transports', async () => {
    const { api } = loadTestApi();
    const values = new Map([['ds_token', 'DS'], ['ds_token_captured_at', Date.now()], ['kimi_token', 'KI'], ['kimi_token_captured_at', Date.now()]]);
    const credentials = new api.CredentialStore({
        getValue: (key, fallback) => values.has(key) ? values.get(key) : fallback,
        setValue: (key, value) => values.set(key, value),
        deleteValue: key => values.delete(key)
    });
    const deepSeekCalls = [];
    const deepSeek = new api.DeepSeekAdapter({
        credentials,
        autoWaitForToken: false,
        location: { pathname: '/a/chat/s/deep' },
        request: async (url, options) => {
            deepSeekCalls.push({ url, options });
            if (url.includes('fetch_page')) return { data: { biz_data: { chat_sessions: [{ id: 'deep', updated_at: 2 }], has_more: false } } };
            return { data: { biz_data: { chat_messages: [] } } };
        }
    });
    assert.equal((await deepSeek.fetchAllSessions()).length, 1);
    await deepSeek.fetchConversation('deep');
    assert.match(deepSeekCalls[0].url, /chat_session\/fetch_page/);
    assert.match(deepSeekCalls[1].url, /history_messages/);

    const doubao = new api.DoubaoAdapter({
        request: async (url) => url.includes('recent_conv')
            ? { downlink_body: { pull_recent_conv_chain_downlink_body: { cells: [{ conversation: { conversation_id: 'dou' } }] } } }
            : { messages: [] }
    });
    assert.equal((await doubao.fetchAllSessions()).length, 1);
    assert.deepEqual(await doubao.fetchConversation('dou'), { messages: [] });

    const kimi = new api.KimiAdapter({ credentials, autoCaptureToken: false, autoWaitForToken: false, request: async (url) => url.includes('ListChats') ? { chats: [{ id: 'ki' }], nextPageToken: '' } : { messages: [] } });
    assert.equal((await kimi.fetchAllSessions()).length, 1);
    assert.deepEqual(await kimi.fetchConversation('ki'), { messages: [] });
});

test('SyncCoordinator performs incremental selection, preserves import payloads, and stops', async () => {
    const { api } = loadTestApi();
    const statuses = [];
    const imports = [];
    const pages = [
        { data: { biz_data: { chat_sessions: [{ id: 'new', updated_at: 30 }, { id: 'old', updated_at: 10 }], has_more: true } } },
        { data: { biz_data: { chat_sessions: [{ id: 'older', updated_at: 5 }], has_more: false } } }
    ];
    const adapter = {
        platform: 'deepseek', needsToken: false,
        fetchSessionPage: async () => pages.shift(),
        _extractSessionsPage: api.DeepSeekAdapter.prototype._extractSessionsPage,
        fetchConversation: async id => ({ id })
    };
    const response = value => ({ ok: true, status: 200, json: async () => value, text: async () => JSON.stringify(value) });
    const bridge = {
        checkServer: async () => ({ state: 'connected', url: 'http://bridge' }),
        request: async path => {
            if (path.includes('sync-status')) return response({ last_updated_at: 20 });
            const payload = JSON.parse(path === '/sessions/import' ? imports.at(-1) || '{}' : '{}');
            void payload;
            return response({ imported: 1, skipped: 0 });
        }
    };
    const ui = { setStatus: text => statuses.push(text), setProgress() {}, setSyncing() {} };
    const coordinator = new api.SyncCoordinator({ adapter, bridgeClient: bridge, ui, detailConcurrency: 1, detailDelayMs: 0 });
    bridge.request = async (path, options = {}) => {
        if (path.includes('sync-status')) return response({ last_updated_at: 20 });
        imports.push(options.body);
        return response({ imported: 1, skipped: 0 });
    };
    const result = await coordinator.sync(false);
    assert.equal(result.imported, 1);
    assert.deepEqual(JSON.parse(imports[0]).sessions.map(item => item.id), ['new']);
    assert.ok(statuses.includes('✅ 导入 1 个, 跳过 0 个'));

    let stopped = false;
    const stopping = new api.SyncCoordinator({
        adapter: { platform: 'kimi', fetchAllSessions: async () => [{ id: 'one' }], fetchConversation: async () => { stopping.stop(); return {}; } },
        bridgeClient: { checkServer: async () => ({ state: 'connected' }), request: async () => response({ last_updated_at: null }) },
        ui: { setStatus() {}, setProgress() {}, setSyncing() {} }, detailConcurrency: 1, detailDelayMs: 0
    });
    const stoppedResult = await stopping.sync(false);
    stopped = stoppedResult.state === 'stopped';
    assert.equal(stopped, true);
});

test('SyncPanel exposes predictable status and syncing transitions with injected DOM', () => {
    const { api } = loadTestApi();
    const ids = ['acm-panel', 'acm-close', 'acm-status-line', 'acm-bar', 'acm-sync-btn', 'acm-full-btn', 'acm-srv'];
    const elements = new Map(ids.map(id => [id, { id, style: {}, dataset: {}, disabled: false }]));
    const root = { innerHTML: '', querySelector: selector => elements.get(selector.slice(1)), remove() {} };
    const document = { createElement: () => root, body: { appendChild() {} } };
    let syncMode = null;
    const panel = new api.SyncPanel({
        document,
        onSync: full => { syncMode = full; },
        onFullSync: full => { syncMode = full; },
        autoMount: true
    });
    panel.setServerStatus({ state: 'connected', url: 'http://bridge' });
    assert.equal(elements.get('acm-sync-btn').disabled, false);
    panel.setSyncing(true);
    assert.equal(elements.get('acm-sync-btn').textContent, '停止同步');
    panel.setSyncing(false);
    assert.equal(elements.get('acm-sync-btn').textContent, '开始同步');
    panel.setStatus('✅ 已完成');
    assert.equal(elements.get('acm-status-line').textContent, '✅ 已完成');
    elements.get('acm-sync-btn').onclick();
    assert.equal(syncMode, false);
    elements.get('acm-full-btn').onclick();
    assert.equal(syncMode, true);
    panel.destroy();
});

test('quota recovery deletes the largest sessions by byte size until under threshold, keeping at least one', () => {
    const { api } = loadTestApi();
    let storedValue = null;
    // 模拟 GM_setValue 的配额语义：序列化后超过 persistenceQuotaBytes 即抛 QuotaExceededError。
    const quota = 600;
    const setValue = (key, value) => {
        if (key !== api.RuntimeConfig.captureStorageKey) return;
        const serialized = JSON.stringify(value);
        if (serialized.length > quota) {
            throw Object.assign(new Error('quota exceeded'), { name: 'QuotaExceededError' });
        }
        storedValue = JSON.parse(serialized);
    };
    const store = new api.CaptureStore({
        getValue: (key, fallback) => key === api.RuntimeConfig.captureStorageKey ? storedValue : fallback,
        setValue,
        config: { persistenceQuotaBytes: quota, maxSessionBytes: 32 * 1024 * 1024 }
    });
    // 直接在内存态构造三个体积差异明显的会话，避免经过 record() 的节流路径。
    const state = store._load();
    state.sessions.big = { updated_at: '2026-01-01', latest_native_history: { response: { body: { data: { biz_data: { chat_messages: [{ fragments: [{ content: 'B'.repeat(420) }] }] } } } } }, completion_exchanges: [], file_exchanges: [], other_exchanges: [], latest_compatibility_history: null };
    state.sessions.mid = { updated_at: '2026-01-01', latest_native_history: { response: { body: { data: { biz_data: { chat_messages: [{ fragments: [{ content: 'M'.repeat(260) }] }] } } } } }, completion_exchanges: [], file_exchanges: [], other_exchanges: [], latest_compatibility_history: null };
    state.sessions.tiny = { updated_at: '2026-01-01', latest_native_history: { response: { body: { data: { biz_data: { chat_messages: [{ fragments: [{ content: 'T'.repeat(40) }] }] } } } } }, completion_exchanges: [], file_exchanges: [], other_exchanges: [], latest_compatibility_history: null };

    // 整体体积超过 quota → 首次 setValue 抛错 → _writeNow 进入配额恢复分支。
    assert.ok(JSON.stringify(state).length > quota, 'precondition: state exceeds quota');
    store._writeNow(state);

    assert.ok(storedValue, 'quota recovery should have persisted a trimmed state');
    const ids = Object.keys(storedValue.sessions);
    assert.ok(ids.length >= 1, 'should keep at least one session');
    assert.equal(storedValue.sessions.big, undefined, 'largest session should be pruned first');
    assert.ok(storedValue.sessions.tiny, 'smallest session should survive');
    assert.ok(JSON.stringify(storedValue).length <= quota, 'persisted state should be under quota');
});

test('quota recovery keeps at least one session even when every session alone exceeds the threshold', () => {
    const { api } = loadTestApi();
    let storedValue = null;
    const quota = 200;
    const setValue = (key, value) => {
        if (key !== api.RuntimeConfig.captureStorageKey) return;
        const serialized = JSON.stringify(value);
        if (serialized.length > quota) {
            throw Object.assign(new Error('quota exceeded'), { name: 'QuotaExceededError' });
        }
        storedValue = JSON.parse(serialized);
    };
    const store = new api.CaptureStore({
        getValue: (key, fallback) => key === api.RuntimeConfig.captureStorageKey ? storedValue : fallback,
        setValue,
        config: { persistenceQuotaBytes: quota, maxSessionBytes: 32 * 1024 * 1024 }
    });
    const state = store._load();
    // 两个会话各自都超过 quota；恢复循环删最大的后，剩 1 个仍超，但 ≥1 约束停止删除。
    state.sessions.huge = { updated_at: '2026-01-01', latest_native_history: { response: { body: { data: { biz_data: { chat_messages: [{ fragments: [{ content: 'H'.repeat(500) }] }] } } } } }, completion_exchanges: [], file_exchanges: [], other_exchanges: [], latest_compatibility_history: null };
    state.sessions.big = { updated_at: '2026-01-01', latest_native_history: { response: { body: { data: { biz_data: { chat_messages: [{ fragments: [{ content: 'G'.repeat(300) }] }] } } } } }, completion_exchanges: [], file_exchanges: [], other_exchanges: [], latest_compatibility_history: null };
    // 该场景下第二次 setValue 仍会抛错（剩余单会话仍超限），_writeNow 会重新抛出——验证不会清空。
    assert.throws(() => store._writeNow(state), { name: 'QuotaExceededError' }, 'should surface error when even one session exceeds quota');
    // 即使最终写失败，内存态仍保留至少 1 个会话（未被清空）。
    const memoryIds = Object.keys(store._load().sessions);
    assert.ok(memoryIds.length >= 1, 'should never drop below one session in memory');
});

test('attachDeepSeekCapture survives a failing flush and still attaches in-memory capture', async () => {
    const warnings = [];
    const { api } = loadTestApi({
        console: { log() {}, error() {}, warn: (...args) => warnings.push(args.map(a => String(a)).join(' ')) }
    });
    const conversation = { data: { biz_data: { chat_messages: [] } } };
    const captureStore = {
        flush: async () => { throw new Error('persist quota hit'); },
        exportSession: sessionId => ({ schema_version: 1, session: { id: sessionId } })
    };
    const result = await api.attachDeepSeekCapture(conversation, 'session-1', captureStore, [{ cite_index: 0 }]);
    assert.equal(result, conversation, 'should return the same conversation');
    assert.equal(result._web_capture.session.id, 'session-1', 'in-memory capture should still be attached');
    assert.ok(warnings.some(w => /flush failed/.test(w)), 'should warn about flush failure');
});

test('record() coalesces multiple captures into one debounced write, and flush forces immediate write', async () => {
    const { api, values } = loadTestApi();
    let writes = 0;
    const store = new api.CaptureStore({
        getValue: (key, fallback) => values.has(key) ? values.get(key) : fallback,
        setValue: (key, value) => { if (key === api.RuntimeConfig.captureStorageKey) writes++; values.set(key, value); },
        config: { persistDebounceMs: 50 }
    });
    // 连续 5 次 record，每次都只调度节流写，���应每条都落盘。
    for (let i = 0; i < 5; i++) {
        await store.record({ id: `c-${i}`, kind: 'completion', sessionId: 'session-1', response: { body: { i } } });
    }
    // 内存态应即时可见（_cachedState 已同步更新）。
    const memoryExchanges = plain(store.exportSession('session-1').session.completion_exchanges.map(item => item.id));
    assert.deepEqual(memoryExchanges, ['c-0', 'c-1', 'c-2', 'c-3', 'c-4'], 'memory state should be synchronous');
    // flush 前磁盘态尚未落盘（节流窗口未到）。
    assert.ok(writes === 0, `should not have persisted before flush, got ${writes}`);
    await store.flush();
    // flush 后立即落盘一次，包含全部 5 条。
    assert.equal(writes, 1, 'flush should coalesce 5 records into a single write');
    const persisted = plain(values.get(api.RuntimeConfig.captureStorageKey));
    assert.deepEqual(persisted.sessions['session-1'].completion_exchanges.map(item => item.id), ['c-0', 'c-1', 'c-2', 'c-3', 'c-4']);
});

test('schema mismatch preserves legacy data under a _legacy_ key with a visible warning', () => {
    const warnings = [];
    const { api, values } = loadTestApi({
        console: { log() {}, error() {}, warn: (...args) => warnings.push(args.map(a => String(a)).join(' ')) }
    });
    const legacy = { schema_version: 0, sessions: { old: { updated_at: '2020-01-01' } }, unassigned: [] };
    values.set(api.RuntimeConfig.captureStorageKey, legacy);
    const store = new api.CaptureStore({
        getValue: (key, fallback) => values.has(key) ? values.get(key) : fallback,
        setValue: (key, value) => values.set(key, value)
    });
    const state = store._load();
    assert.equal(state.schema_version, api.RuntimeConfig.captureSchemaVersion, 'should reset to empty state with current schema');
    assert.deepEqual(plain(state.sessions), {}, 'should reset sessions to empty');
    const legacyKeys = [...values.keys()].filter(k => k.startsWith(api.RuntimeConfig.captureStorageKey + '_legacy_'));
    assert.equal(legacyKeys.length, 1, 'should write exactly one _legacy_ backup key');
    assert.deepEqual(plain(values.get(legacyKeys[0])), legacy, 'legacy backup should preserve original data');
    assert.ok(warnings.some(w => /_legacy_/.test(w) && /GM_getValue/.test(w)), 'should warn naming the legacy key and how to recover it');
});

test('pollServer collapses concurrent checks into one in-flight request', async () => {
    const { api } = loadTestApi();
    const ids = ['acm-panel', 'acm-close', 'acm-status-line', 'acm-bar', 'acm-sync-btn', 'acm-full-btn', 'acm-srv'];
    const elements = new Map(ids.map(id => [id, { id, style: {}, dataset: {}, disabled: false }]));
    const root = { innerHTML: '', querySelector: selector => elements.get(selector.slice(1)), remove() {} };
    const document = { createElement: () => root, body: { appendChild() {} } };
    let checks = 0;
    let release;
    const gate = new Promise(resolve => { release = resolve; });
    const panel = new api.SyncPanel({
        document,
        autoMount: false,
        onCheckServer: async () => {
            checks++;
            await gate;
            return { state: 'connected', url: 'http://bridge' };
        }
    });
    const first = panel.pollServer();
    const second = panel.pollServer();
    assert.equal(checks, 1, 'second concurrent poll should be rejected before issuing a request');
    assert.equal(await second, null, 'concurrent poll should return null without a connection');
    release();
    const connection = await first;
    assert.equal(connection.state, 'connected');
    assert.equal(checks, 1, 'no extra health check should have been issued');
    // 首次完成后守卫应复位，允许下一次轮询真正执行。
    const third = panel.pollServer();
    release();
    await third;
    assert.equal(checks, 2, 'guard should reset after the in-flight check completes');
});

test('fetchAllSessions stops paging when the cursor stops advancing', async () => {
    const { api } = loadTestApi();
    const values = new Map([['ds_token', 'DS'], ['ds_token_captured_at', Date.now()]]);
    const credentials = new api.CredentialStore({
        getValue: (key, fallback) => values.has(key) ? values.get(key) : fallback,
        setValue: (key, value) => values.set(key, value),
        deleteValue: key => values.delete(key)
    });
    let pageRequests = 0;
    // 所有会话 updated_at 相同且 has_more=true：下一页永远返回同一列表，游标停滞。
    const stalledPage = () => {
        pageRequests++;
        return { data: { biz_data: { chat_sessions: [{ id: `s-${pageRequests}-a`, updated_at: 100 }, { id: `s-${pageRequests}-b`, updated_at: 100 }], has_more: true } } };
    };
    const adapter = new api.DeepSeekAdapter({
        credentials,
        autoWaitForToken: false,
        location: { pathname: '/a/chat/s/deep' },
        request: async () => stalledPage()
    });
    const sessions = await adapter.fetchAllSessions();
    assert.ok(pageRequests <= 2, `should stop after first+stalled page, got ${pageRequests} requests`);
    assert.equal(sessions.length, 4, 'both pages of sessions should still be collected');
});

test('fetchDetailsAndPush retries a session that fails once then succeeds, and a twice-failed session lands in failed', async () => {
    const { api } = loadTestApi();
    const attempts = {};
    const adapter = {
        platform: 'deepseek', needsToken: false,
        fetchConversation: async id => {
            attempts[id] = (attempts[id] || 0) + 1;
            if (id === 'flaky' && attempts[id] === 1) throw new Error('transient');
            if (id === 'doomed') throw new Error('permanent');
            return { id, messages: [] };
        }
    };
    const response = value => ({ ok: true, status: 200, json: async () => value, text: async () => JSON.stringify(value) });
    const bridge = { request: async () => response({ imported: 1, skipped: 0 }) };
    const coordinator = new api.SyncCoordinator({
        adapter, bridgeClient: bridge,
        ui: { setStatus() {}, setProgress() {}, setSyncing() {} },
        detailConcurrency: 1, detailDelayMs: 0
    });
    const result = await coordinator.fetchDetailsAndPush([{ id: 'flaky' }, { id: 'doomed' }, { id: 'ok' }]);
    assert.deepEqual(plain(result.sessions.map(s => s.id)), ['flaky', 'ok'], 'flaky should recover via retry, ok should pass first try');
    assert.deepEqual(plain(result.failed.map(f => f.id)), ['doomed'], 'only the twice-failed session should be in failed');
    assert.equal(attempts.flaky, 2, 'flaky should have been attempted twice (1 fail + 1 retry)');
    assert.equal(attempts.doomed, 2, 'doomed should have been attempted twice (both fail)');
    assert.equal(attempts.ok, 1, 'ok should have been attempted once');
});

test('session list and detail fetches forward the abort signal to the transport', async () => {
    const { api } = loadTestApi();
    const values = new Map([['ds_token', 'DS'], ['ds_token_captured_at', Date.now()]]);
    const credentials = new api.CredentialStore({
        getValue: (key, fallback) => values.has(key) ? values.get(key) : fallback,
        setValue: (key, value) => values.set(key, value),
        deleteValue: key => values.delete(key)
    });
    const signals = [];
    const adapter = new api.DeepSeekAdapter({
        credentials,
        autoWaitForToken: false,
        location: { pathname: '/a/chat/s/deep' },
        request: async (url, options) => {
            signals.push(options?.signal ?? null);
            return { data: { biz_data: { chat_sessions: [{ id: 's1', updated_at: 1 }], has_more: false } } };
        }
    });
    const controller = new AbortController();
    await adapter.fetchAllSessions({ signal: controller.signal });
    assert.ok(signals.length >= 1, 'list paging should reach the transport');
    assert.ok(signals.every(signal => signal === controller.signal), 'list paging must forward the abort signal to every page request');
    signals.length = 0;
    await adapter.fetchConversation('s1', { signal: controller.signal });
    assert.equal(signals.length, 1, 'detail fetch should reach the transport');
    assert.equal(signals[0], controller.signal, 'detail fetch must forward the abort signal');
});

const encodedImportBytes = (platform, batch) =>
    new TextEncoder().encode(JSON.stringify({ platform, sessions: batch })).byteLength;

test('buildImportBatches respects count and UTF-8 body limits', () => {
    const { api } = loadTestApi();
    const sessions = [
        { id: 'a', title: '你'.repeat(20) },
        { id: 'b', title: '好'.repeat(20) },
        { id: 'c', title: 'ok' },
    ];

    const byCount = api.buildImportBatches('deepseek', sessions, { maxItems: 2, maxBodyBytes: 1024 * 1024 });
    assert.equal(byCount.oversized.length, 0);
    assert.deepEqual(plain(byCount.batches.map(batch => batch.map(item => item.id))), [['a', 'b'], ['c']], 'item count must cap a batch');

    // 20 个中文字符 = 60 UTF-8 字节：用 string.length 判定会严重低估，只有实测字节才会在此切分。
    const maxBodyBytes = 210;
    assert.ok(encodedImportBytes('deepseek', sessions.slice(0, 2)) <= maxBodyBytes, 'fixture: first two must fit');
    assert.ok(encodedImportBytes('deepseek', sessions) > maxBodyBytes, 'fixture: all three must exceed the limit');
    const { batches, oversized } = api.buildImportBatches('deepseek', sessions, { maxItems: 100, maxBodyBytes });
    assert.equal(oversized.length, 0);
    assert.ok(batches.length > 1, 'byte limit must force a split');
    assert.ok(batches.every(batch => encodedImportBytes('deepseek', batch) <= maxBodyBytes), 'every batch envelope must stay under the byte limit');
    assert.deepEqual(plain(batches.flat().map(item => item.id)), ['a', 'b', 'c'], 'no session may be dropped or reordered');
});

test('buildImportBatches moves a single oversized session to oversized and keeps batching the rest', () => {
    const { api } = loadTestApi();
    const sessions = [
        { id: 'small1', title: 'x' },
        { id: 'huge', title: '测'.repeat(500) },
        { id: 'small2', title: 'y' },
    ];
    const maxBodyBytes = 200;
    const { batches, oversized } = api.buildImportBatches('p', sessions, { maxItems: 100, maxBodyBytes });
    assert.deepEqual(plain(oversized.map(item => item.id)), ['huge'], 'the session that cannot fit alone must be reported, not sent');
    assert.deepEqual(plain(batches.flat().map(item => item.id)), ['small1', 'small2'], 'remaining sessions must still be batched');
    assert.ok(batches.every(batch => encodedImportBytes('p', batch) <= maxBodyBytes));
});

test('fetchDetailsAndPush sends batches and keeps partial success when one batch fails', async () => {
    const { api } = loadTestApi();
    const adapter = {
        platform: 'deepseek', needsToken: false,
        fetchConversation: async id => ({ id, messages: [] })
    };
    const bodies = [];
    const bridge = {
        request: async (path, options = {}) => {
            bodies.push(options.body);
            if (bodies.length === 2) {
                return { ok: false, status: 413, json: async () => ({}), text: async () => 'payload too large' };
            }
            return { ok: true, status: 200, json: async () => ({ imported: 2, skipped: 1 }), text: async () => '{}' };
        }
    };
    const maxBodyBytes = 4096;
    const coordinator = new api.SyncCoordinator({
        adapter, bridgeClient: bridge,
        ui: { setStatus() {}, setProgress() {}, setSyncing() {} },
        detailConcurrency: 1, detailDelayMs: 0,
        config: { importBatchMaxItems: 2, importBatchMaxBodyBytes: maxBodyBytes }
    });

    const result = await coordinator.fetchDetailsAndPush([{ id: 's1' }, { id: 's2' }, { id: 's3' }]);

    assert.equal(bodies.length, 2, 'three sessions with maxItems=2 must become exactly two requests');
    assert.ok(bodies.every(body => new TextEncoder().encode(body).byteLength <= maxBodyBytes), 'no request body may exceed the configured limit');
    assert.deepEqual(plain(result.sessions.map(s => s.id)), ['s1', 's2'], 'only sessions accepted by an ok batch count as synced');
    assert.equal(result.imported, 2, 'imported must accumulate from the successful batch only');
    assert.equal(result.skipped, 1);
    assert.deepEqual(plain(result.failed.map(f => ({ id: f.id, stage: f.stage }))), [{ id: 's3', stage: 'import' }], 'the failed batch must be reported with the import stage');
});

test('fetchDetailsAndPush throws when every batch fails', async () => {
    const { api } = loadTestApi();
    const adapter = { platform: 'deepseek', needsToken: false, fetchConversation: async id => ({ id }) };
    const bridge = { request: async () => ({ ok: false, status: 413, json: async () => ({}), text: async () => 'too large' }) };
    const coordinator = new api.SyncCoordinator({
        adapter, bridgeClient: bridge,
        ui: { setStatus() {}, setProgress() {}, setSyncing() {} },
        detailConcurrency: 1, detailDelayMs: 0
    });
    await assert.rejects(() => coordinator.fetchDetailsAndPush([{ id: 's1' }]), /413/);
});

const okImport = (imported, skipped = 0) => ({ ok: true, status: 200, json: async () => ({ imported, skipped }), text: async () => '{}' });
const makePushCoordinator = (api, bridge, config = { importBatchMaxItems: 1, importBatchMaxBodyBytes: 4096 }) => new api.SyncCoordinator({
    adapter: { platform: 'deepseek', needsToken: false, fetchConversation: async id => ({ id, messages: [] }) },
    bridgeClient: bridge,
    ui: { setStatus() {}, setProgress() {}, setSyncing() {} },
    detailConcurrency: 1, detailDelayMs: 0,
    config
});

test('fetchDetailsAndPush records a batch the server accepted even if stop() lands while the response is in flight', async () => {
    const { api } = loadTestApi();
    let coordinator;
    let requests = 0;
    const bridge = {
        request: async () => {
            requests++;
            // The server has already imported the batch; the user hits stop before the response is handled.
            coordinator.stop();
            return okImport(1);
        }
    };
    coordinator = makePushCoordinator(api, bridge);
    const result = await coordinator.fetchDetailsAndPush([{ id: 's1' }, { id: 's2' }]);
    assert.equal(result.state, 'stopped');
    assert.equal(requests, 1, 'no further batch may be sent after stop');
    assert.deepEqual(plain(result.sessions.map(s => s.id)), ['s1'], 'a batch accepted by the server must be reported as synced even when stopped right after');
    assert.equal(result.imported, 1, 'imported must be accumulated for the accepted batch');
});

test('fetchDetailsAndPush treats a thrown transport error as an import-stage failure of that batch only', async () => {
    const { api } = loadTestApi();
    let requests = 0;
    const bridge = {
        request: async () => {
            requests++;
            if (requests === 1) throw new Error('network down');
            return okImport(1);
        }
    };
    const coordinator = makePushCoordinator(api, bridge);
    const result = await coordinator.fetchDetailsAndPush([{ id: 's1' }, { id: 's2' }]);
    assert.equal(requests, 2, 'the second batch must still be sent after a transport error');
    assert.deepEqual(plain(result.failed.map(f => ({ id: f.id, stage: f.stage }))), [{ id: 's1', stage: 'import' }]);
    assert.match(result.failed[0].error, /network down/);
    assert.deepEqual(plain(result.sessions.map(s => s.id)), ['s2']);
    assert.equal(result.imported, 1, 'imported counts only the batch that succeeded');
});

test('fetchDetailsAndPush reports a single oversized session without sending it and imports the rest', async () => {
    const { api } = loadTestApi();
    const bodies = [];
    const bridge = { request: async (path, options = {}) => { bodies.push(options.body); return okImport(1); } };
    const coordinator = makePushCoordinator(api, bridge, { importBatchMaxItems: 100, importBatchMaxBodyBytes: 200 });
    const result = await coordinator.fetchDetailsAndPush([{ id: 'small1' }, { id: 'huge', title: 'x'.repeat(400) }, { id: 'small2' }]);
    assert.deepEqual(plain(result.failed.map(f => ({ id: f.id, stage: f.stage }))), [{ id: 'huge', stage: 'oversized' }]);
    assert.ok(bodies.every(body => !body.includes('"huge"')), 'the oversized session must never be sent');
    const sent = bodies.map(body => JSON.parse(body).sessions.map(s => s.id));
    assert.deepEqual(sent.flat(), ['small1', 'small2'], 'remaining sessions must be sent');
    assert.equal(bodies.length, sent.length, 'request count equals the number of non-oversized batches');
    assert.deepEqual(plain(result.sessions.map(s => s.id)), ['small1', 'small2']);
    assert.equal(result.imported, bodies.length);
});

test('fetchDetailsAndPush stops between batches and keeps the first accepted batch', async () => {
    const { api } = loadTestApi();
    let coordinator;
    let requests = 0;
    let stopCalls = 0;
    // The first batch's response is fully delivered; stop() lands while its body is being read,
    // which is the last hook before the loop's between-batch stopped check.
    const bridge = {
        request: async () => {
            requests++;
            return {
                ok: true, status: 200, text: async () => '{}',
                json: async () => { stopCalls++; coordinator.stop(); return { imported: 1, skipped: 0 }; }
            };
        }
    };
    coordinator = makePushCoordinator(api, bridge);
    const result = await coordinator.fetchDetailsAndPush([{ id: 's1' }, { id: 's2' }]);
    assert.equal(result.state, 'stopped');
    assert.deepEqual(plain(result.sessions.map(s => s.id)), ['s1']);
    assert.equal(result.imported, 1);
    assert.equal(requests, 1, 'only the first batch may reach the bridge');
    assert.equal(stopCalls, 1);
});

// ===== Persistent retry queue (audit #27): failed sessions must survive the cursor and a page refresh =====

const RETRY_KEY = 'sync_retry_queue_v1:deepseek';

// Incremental deepseek adapter whose list page is served by `pages` and whose detail fetch fails for ids in `failing`.
const makeRetryAdapter = (api, pages, failing = new Set()) => ({
    platform: 'deepseek', needsToken: false,
    fetchSessionPage: async () => pages.shift() || { data: { biz_data: { chat_sessions: [], has_more: false } } },
    _extractSessionsPage: api.DeepSeekAdapter.prototype._extractSessionsPage,
    fetchConversation: async id => {
        if (failing.has(id)) throw new Error(`detail boom ${id}`);
        return { id, messages: [{ role: 'user', content: 'hello' }] };
    }
});
const page = (sessions, hasMore = false) => ({ data: { biz_data: { chat_sessions: sessions, has_more: hasMore } } });
const makeRetryBridge = (cursorRef, bodies) => ({
    checkServer: async () => ({ state: 'connected', url: 'http://bridge' }),
    request: async (path, options = {}) => {
        if (path.includes('sync-status')) return { ok: true, status: 200, json: async () => ({ last_updated_at: cursorRef.value }), text: async () => '{}' };
        bodies.push(options.body);
        return okImport(JSON.parse(options.body).sessions.length);
    }
});
const quietUi = { setStatus() {}, setProgress() {}, setSyncing() {} };

test('SyncRetryStore keeps list-level metadata only, truncates messages, counts attempts, and removes ids', () => {
    const { api } = loadTestApi();
    const storage = new Map();
    const store = new api.SyncRetryStore('deepseek', (key, fallback) => storage.has(key) ? storage.get(key) : fallback, (key, value) => storage.set(key, value));
    assert.deepEqual(plain(store.load()), {}, 'an empty store loads as an empty entries object');
    assert.deepEqual(plain(store.sessions()), []);

    const session = { id: 'a', updated_at: 120, title: 'A', _conversation: { messages: [{ content: 'secret body' }] } };
    store.upsert(session, 'detail', 'x'.repeat(700));
    const saved = storage.get(RETRY_KEY);
    assert.equal(saved.version, 1);
    const entry = saved.entries.a;
    assert.equal(entry.id, 'a');
    assert.equal(entry.updated_at, 120);
    assert.equal(entry.stage, 'detail');
    assert.equal(entry.attempts, 1);
    assert.equal(entry.message.length, 500, 'error message must be truncated to 500 chars');
    assert.deepEqual(plain(entry.session), { id: 'a', updated_at: 120, title: 'A' }, 'the conversation body must never be persisted');
    assert.equal(JSON.stringify(saved).includes('secret body'), false);

    store.upsert({ id: 'a', updated_at: 120 }, 'import', 'again');
    assert.equal(storage.get(RETRY_KEY).entries.a.attempts, 2, 'attempts increments on repeated failures');
    assert.equal(storage.get(RETRY_KEY).entries.a.stage, 'import', 'the latest stage wins');

    store.upsert({ id: 'b', updated_at: 5 }, 'detail', 'b failed');
    assert.deepEqual(plain(store.sessions().map(s => s.id).sort()), ['a', 'b']);
    store.remove(['a', 'missing']);
    assert.deepEqual(plain(Object.keys(storage.get(RETRY_KEY).entries)), ['b'], 'remove persists immediately');
});

test('SyncRetryStore falls back to an empty retry queue when the stored value is corrupt', () => {
    const { api } = loadTestApi();
    const storage = new Map();
    const getValue = (key, fallback) => storage.has(key) ? storage.get(key) : fallback;
    const setValue = (key, value) => storage.set(key, value);
    for (const corrupt of ['garbage', 42, null, { version: 2, entries: { a: {} } }, { version: 1, entries: 'nope' }, { version: 1 }, [1, 2]]) {
        storage.set(RETRY_KEY, corrupt);
        const store = new api.SyncRetryStore('deepseek', getValue, setValue);
        assert.deepEqual(plain(store.load()), {}, `corrupt value ${JSON.stringify(corrupt)} must load as empty`);
        assert.deepEqual(plain(store.sessions()), []);
    }
    const store = new api.SyncRetryStore('deepseek', getValue, setValue);
    store.upsert({ id: 'a', updated_at: 1 }, 'detail', 'boom');
    assert.deepEqual(Object.keys(storage.get(RETRY_KEY).entries), ['a'], 'a corrupt value is replaced by a fresh v1 queue on the next write');
    assert.equal(storage.get(RETRY_KEY).version, 1);
});

test('retry queue persists a detail failure to GM storage and a reloaded coordinator still selects it past the cursor', async () => {
    const { api, values: storage } = loadTestApi();
    const bodies = [];
    const cursor = { value: 100 };
    const failing = new Set(['failed-a']);
    const first = new api.SyncCoordinator({
        adapter: makeRetryAdapter(api, [page([{ id: 'ok-b', updated_at: 150 }, { id: 'failed-a', updated_at: 120 }])], failing),
        bridgeClient: makeRetryBridge(cursor, bodies), ui: quietUi, detailConcurrency: 1, detailDelayMs: 0
    });
    const result = await first.sync(false);
    assert.equal(result.imported, 1);
    assert.deepEqual(plain(result.failed.map(f => ({ id: f.id, stage: f.stage }))), [{ id: 'failed-a', stage: 'detail' }]);
    assert.deepEqual(JSON.parse(bodies[0]).sessions.map(s => s.id), ['ok-b']);

    const saved = storage.get(RETRY_KEY);
    assert.equal(saved?.version, 1, 'the retry queue must be persisted via the default GM storage');
    assert.equal(saved.entries['failed-a'].attempts, 1);
    assert.equal(saved.entries['failed-a'].stage, 'detail');
    assert.deepEqual(plain(saved.entries['failed-a'].session), { id: 'failed-a', updated_at: 120 });
    assert.equal(Object.keys(saved.entries).length, 1, 'the successful session must not be queued');

    // Page refresh: a brand-new coordinator over the same GM storage; the server cursor advanced to B (200 > 120),
    // so the list page no longer surfaces A.
    const reloaded = new api.SyncCoordinator({
        adapter: makeRetryAdapter(api, [page([{ id: 'ok-b', updated_at: 150 }])], failing),
        bridgeClient: makeRetryBridge(cursor, bodies), ui: quietUi, detailConcurrency: 1, detailDelayMs: 0
    });
    const retried = await reloaded.selectSessions('200');
    assert.deepEqual(plain(retried.map(item => item.id)), ['failed-a']);
    assert.equal(storage.get(RETRY_KEY).entries['failed-a'].attempts, 1);
});

test('cursor advances past failures yet later syncs keep retrying the queued session until the server accepts it', async () => {
    const { api, values: storage } = loadTestApi();
    const bodies = [];
    const cursor = { value: 100 };
    const failing = new Set(['failed-a']);
    const pages = [page([{ id: 'ok-b', updated_at: 150 }, { id: 'failed-a', updated_at: 120 }])];
    const make = () => new api.SyncCoordinator({
        adapter: makeRetryAdapter(api, pages, failing),
        bridgeClient: makeRetryBridge(cursor, bodies), ui: quietUi, detailConcurrency: 1, detailDelayMs: 0
    });
    await make().sync(false);
    assert.deepEqual(Object.keys(storage.get(RETRY_KEY).entries), ['failed-a']);

    // Round 2: backend cursor moved to B; the list page is empty beyond the cursor, so the queued A is the only
    // candidate. It fails again -> attempts 2. (Every candidate failing keeps the pre-existing "all detail fetches
    // failed" error result; what matters here is that A was still selected and stays queued.)
    cursor.value = 150;
    pages.push(page([{ id: 'ok-b', updated_at: 150 }]));
    const second = await make().sync(false);
    assert.equal(second.state, 'error');
    assert.match(String(second.error?.message), /detail boom failed-a/);
    assert.equal(storage.get(RETRY_KEY).entries['failed-a'].attempts, 2);
    assert.equal(bodies.length, 1, 'nothing new to push in round 2');

    // Round 3: A recovers; it is pushed and removed from the queue.
    failing.clear();
    pages.push(page([{ id: 'ok-b', updated_at: 150 }]));
    const third = await make().sync(false);
    assert.equal(third.imported, 1);
    assert.deepEqual(JSON.parse(bodies[1]).sessions.map(s => s.id), ['failed-a']);
    assert.deepEqual(plain(storage.get(RETRY_KEY).entries), {}, 'an accepted session leaves the retry queue');

    // Round 4: nothing pending anymore.
    pages.push(page([{ id: 'ok-b', updated_at: 150 }]));
    const fourth = await make().sync(false);
    assert.equal(fourth.imported, 0);
    assert.equal(bodies.length, 2);
});

test('retry queue records import-stage batch failures, skips oversized sessions, and removes accepted or skipped ones', async () => {
    const { api, values: storage } = loadTestApi();
    let requests = 0;
    const bridge = {
        request: async (path, options = {}) => {
            requests++;
            const ids = JSON.parse(options.body).sessions.map(s => s.id);
            if (ids.includes('http-fail')) return { ok: false, status: 500, json: async () => ({}), text: async () => 'server exploded ' + 'y'.repeat(600) };
            if (ids.includes('skipped-only')) return okImport(0, 1);
            return okImport(ids.length);
        }
    };
    const coordinator = makePushCoordinator(api, bridge, { importBatchMaxItems: 1, importBatchMaxBodyBytes: 200 });
    // Pre-seed the queue so the successful/skipped sessions prove removal.
    coordinator.retryStore.upsert({ id: 'ok', updated_at: 1 }, 'detail', 'earlier');
    coordinator.retryStore.upsert({ id: 'skipped-only', updated_at: 1 }, 'import', 'earlier');
    const result = await coordinator.fetchDetailsAndPush([
        { id: 'ok', updated_at: 1 }, { id: 'http-fail', updated_at: 2 }, { id: 'skipped-only', updated_at: 3 }, { id: 'huge', title: 'x'.repeat(400) }
    ]);
    assert.equal(requests, 3, 'the oversized session is never sent');
    assert.deepEqual(plain(result.failed.map(f => ({ id: f.id, stage: f.stage }))).sort((a, b) => a.id.localeCompare(b.id)), [
        { id: 'http-fail', stage: 'import' }, { id: 'huge', stage: 'oversized' }
    ]);
    const entries = storage.get(RETRY_KEY).entries;
    assert.deepEqual(Object.keys(entries), ['http-fail'], 'only the HTTP-failed session is queued; oversized is not, accepted/skipped are removed');
    assert.equal(entries['http-fail'].stage, 'import');
    assert.equal(entries['http-fail'].attempts, 1);
    assert.equal(entries['http-fail'].message.length, 500);
    assert.match(entries['http-fail'].message, /500/);
    assert.deepEqual(plain(entries['http-fail'].session), { id: 'http-fail', updated_at: 2 }, 'the fetched conversation must not be persisted');
});

test('selectSessions merges retry entries with normal candidates, dedupes by id, and prefers the fresher list metadata', async () => {
    const { api } = loadTestApi();
    const storage = new Map();
    const getValue = (key, fallback) => storage.has(key) ? storage.get(key) : fallback;
    const setValue = (key, value) => storage.set(key, value);
    const store = new api.SyncRetryStore('deepseek', getValue, setValue);
    store.upsert({ id: 'dup', updated_at: 120, title: 'stale' }, 'detail', 'boom');
    store.upsert({ id: 'only-retry', updated_at: 50 }, 'import', 'boom');

    const coordinator = new api.SyncCoordinator({
        adapter: makeRetryAdapter(api, [page([{ id: 'dup', updated_at: 300, title: 'fresh' }, { id: 'new', updated_at: 250 }])]),
        bridgeClient: {}, ui: quietUi, getValue, setValue
    });
    const selected = await coordinator.selectSessions('200');
    assert.deepEqual(plain(selected.map(s => s.id)), ['dup', 'new', 'only-retry'], 'normal candidates first, then queued sessions not already present');
    assert.equal(selected[0].title, 'fresh', 'the normal candidate keeps its newer list metadata');
    assert.equal(selected.filter(s => s.id === 'dup').length, 1);

    // The full (no cursor) path merges too and stays idempotent.
    const full = new api.SyncCoordinator({
        adapter: { platform: 'deepseek', fetchAllSessions: async () => [{ id: 'dup', updated_at: 300, title: 'fresh' }] },
        bridgeClient: {}, ui: quietUi, getValue, setValue
    });
    assert.deepEqual(plain((await full.selectSessions(null)).map(s => s.id)), ['dup', 'only-retry']);
});

test('platform retry isolation: a deepseek retry queue never leaks into a kimi coordinator and vice versa', async () => {
    const { api, values: storage } = loadTestApi();
    const kimiKey = 'sync_retry_queue_v1:kimi';
    const cursor = { value: 100 };
    const bodies = [];
    const failing = new Set(['failed-a']);
    const deepseek = new api.SyncCoordinator({
        adapter: makeRetryAdapter(api, [page([{ id: 'failed-a', updated_at: 120 }, { id: 'ok-b', updated_at: 150 }])], failing),
        bridgeClient: makeRetryBridge(cursor, bodies), ui: quietUi, detailConcurrency: 1, detailDelayMs: 0
    });
    await deepseek.sync(false);
    assert.deepEqual(Object.keys(storage.get(RETRY_KEY).entries), ['failed-a']);
    assert.equal(storage.has(kimiKey), false, 'a deepseek failure must not create a kimi queue');

    const kimi = new api.SyncCoordinator({
        adapter: { platform: 'kimi', fetchAllSessions: async () => [{ id: 'kimi-1' }] },
        bridgeClient: makeRetryBridge(cursor, bodies), ui: quietUi
    });
    assert.deepEqual(plain((await kimi.selectSessions('200')).map(s => s.id)), ['kimi-1'], 'kimi selection ignores the deepseek queue');
    assert.equal(storage.has(kimiKey), false);

    kimi.retryStore.upsert({ id: 'kimi-failed' }, 'detail', 'boom');
    assert.deepEqual(Object.keys(storage.get(kimiKey).entries), ['kimi-failed']);
    assert.deepEqual(Object.keys(storage.get(RETRY_KEY).entries), ['failed-a'], 'the deepseek queue is untouched by kimi writes');
    assert.deepEqual(plain((await deepseek.selectSessions('200')).map(s => s.id)), ['failed-a'], 'deepseek selection ignores the kimi queue');
});

