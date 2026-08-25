// ==UserScript==
// @name         AI Chat Memory - 多平台导出
// @namespace    ai-chat-memory
// @version      1.1.0
// @description  跨平台AI对话导出工具，支持同步到本地服务
// @author       AI Chat Memory
// @match        https://chat.deepseek.com/*
// @match        https://www.doubao.com/*
// @match        https://kimi.com/*
// @match        https://www.kimi.com/*
// @run-at       document-start
// @grant        GM_setValue
// @grant        GM_getValue
// @grant        GM_deleteValue
// @grant        GM_registerMenuCommand
// @grant        GM_xmlhttpRequest
// @grant        unsafeWindow
// ==/UserScript==

(function() {
    'use strict';

    const RuntimeConfig = Object.freeze({
        captureSchemaVersion: 1,
        captureStorageKey: 'deepseek_web_capture_v1',
        referenceStorageKey: 'deepseek_reference_cache_v1',
        defaultBridgeUrl: 'http://localhost:19820/api/v1',
        bridgeUrlKey: 'bridge_url',
        bridgeSecretKey: 'bridge_secret',
        tokenTtlMs: 24 * 60 * 60 * 1000,
        maxResponseBytes: 16 * 1024 * 1024,
        maxSessionBytes: 32 * 1024 * 1024,
        maxCompletionExchanges: 128,
        maxFileExchanges: 128,
        maxOtherExchanges: 64,
        maxUnassignedExchanges: 64
    });

    const JsonTools = Object.freeze({
        safeParse(value) {
            if (typeof value !== 'string') return value;
            try { return JSON.parse(value); } catch { return null; }
        },
        clone(value) {
            if (value === undefined) return undefined;
            if (typeof structuredClone === 'function') {
                try { return structuredClone(value); } catch {}
            }
            try { return JSON.parse(JSON.stringify(value)); } catch { return String(value); }
        },
        byteLength(value) {
            const text = typeof value === 'string' ? value : JSON.stringify(value ?? null);
            return typeof TextEncoder === 'function' ? new TextEncoder().encode(text).length : text.length;
        },
        isPlainObject(value) {
            if (!value || Object.prototype.toString.call(value) !== '[object Object]') return false;
            const prototype = Object.getPrototypeOf(value);
            return prototype === null || prototype === Object.prototype;
        }
    });

    const CaptureRedactor = (() => {
        const sensitiveNames = /^(authorization|proxy-authorization|cookie|set-cookie|secret|token|access_token|refresh_token|x-settings-token|device_id|did|device_token|fingerprint|password|email|phone|mobile)$/i;
        const sensitiveQueryNames = /^(did|device_id|token|access_token|refresh_token|secret|signature|sign|auth|key|password|email|phone)$/i;
        const powCredential = /pow.*(response|answer)|(?:response|answer).*pow/i;

        function isSensitive(name) {
            return sensitiveNames.test(String(name)) || powCredential.test(String(name));
        }

        function redactHeaders(headers) {
            const result = {};
            if (!headers) return result;
            const entries = typeof headers.entries === 'function' ? [...headers.entries()] : Object.entries(headers);
            for (const [name, value] of entries) {
                if (!isSensitive(name)) result[String(name).toLowerCase()] = String(value);
            }
            return result;
        }

        function redactUrl(value) {
            try {
                const url = new URL(String(value), 'https://chat.deepseek.com');
                for (const name of [...url.searchParams.keys()]) {
                    if (sensitiveQueryNames.test(name) || powCredential.test(name)) {
                        url.searchParams.set(name, '{REDACTED}');
                    }
                }
                return url.toString();
            } catch {
                return String(value || '');
            }
        }

        function redactValue(value, seen = new WeakSet()) {
            if (Array.isArray(value)) return value.map(item => redactValue(item, seen));
            if (!value || typeof value !== 'object') return value;
            if (seen.has(value)) return '{CIRCULAR}';
            seen.add(value);
            const result = {};
            for (const [name, item] of Object.entries(value)) {
                if (name === 'dataRaw' || name === 'dataJson' || name === 'capturedAt') continue;
                result[name] = isSensitive(name) ? '{REDACTED}' : redactValue(item, seen);
            }
            if (Object.prototype.hasOwnProperty.call(result, 'data_raw')) {
                Object.defineProperties(result, {
                    dataRaw: { enumerable: Boolean(globalThis.__AI_CHAT_MEMORY_TEST_MODE__), get: () => result.data_raw },
                    dataJson: { enumerable: Boolean(globalThis.__AI_CHAT_MEMORY_TEST_MODE__), get: () => result.data_json },
                    capturedAt: { enumerable: Boolean(globalThis.__AI_CHAT_MEMORY_TEST_MODE__), get: () => result.captured_at }
                });
            }
            return result;
        }

        function redactRawString(text) {
            if (typeof text !== 'string' || !text) return text;
            return text
                .replace(/"(authorization|token|access_token|refresh_token|secret|password|device_id|did|device_token|cookie|email|phone)"\s*:\s*"[^"]+"/gi, '"$1":"{REDACTED}"')
                .replace(/(Bearer\s+)[a-zA-Z0-9_\-\.]{16,}/gi, '$1{REDACTED}')
                .replace(/(pow.*(?:response|answer))\s*:\s*"[^"]+"/gi, '"$1":"{REDACTED}"');
        }

        function redactExchange(exchange) {
            const result = redactValue(exchange);
            if (result?.request) {
                result.request.url = redactUrl(result.request.url);
                result.request.headers = redactHeaders(exchange?.request?.headers);
                if (typeof result.request.data_raw === 'string') {
                    result.request.data_raw = redactRawString(result.request.data_raw);
                }
                if (typeof result.request.dataRaw === 'string') {
                    result.request.dataRaw = redactRawString(result.request.dataRaw);
                }
            }
            if (result?.response) {
                result.response.headers = redactHeaders(exchange?.response?.headers);
                if (typeof result.response.data_raw === 'string') {
                    result.response.data_raw = redactRawString(result.response.data_raw);
                }
                if (typeof result.response.dataRaw === 'string') {
                    result.response.dataRaw = redactRawString(result.response.dataRaw);
                }
                const events = result.response.sse_events || result.response.sseEvents;
                if (Array.isArray(events)) {
                    const redactedEvents = events.map(ev => {
                        const nextEv = { ...ev };
                        if (typeof nextEv.data_raw === 'string') {
                            nextEv.data_raw = redactRawString(nextEv.data_raw);
                        }
                        if (typeof nextEv.dataRaw === 'string') {
                            nextEv.dataRaw = redactRawString(nextEv.dataRaw);
                        }
                        if (nextEv.data_json) {
                            nextEv.data_json = redactValue(nextEv.data_json);
                        }
                        if (nextEv.dataJson) {
                            nextEv.dataJson = redactValue(nextEv.dataJson);
                        }
                        return nextEv;
                    });
                    if (result.response.sse_events) result.response.sse_events = redactedEvents;
                    if (result.response.sseEvents) result.response.sseEvents = redactedEvents;
                }
            }
            return result;
        }

        return Object.freeze({ redactExchange, redactHeaders, redactUrl, redactValue, redactRawString });
    })();

    const SseParser = Object.freeze({
        parse(text, now = () => new Date().toISOString()) {
            const events = [];
            let eventName = 'message';
            let dataLines = [];
            const flush = () => {
                if (!dataLines.length && eventName === 'message') return;
                const dataRaw = dataLines.join('\n');
                const event = {
                    event: eventName || 'message',
                    data_raw: dataRaw,
                    data_json: JsonTools.safeParse(dataRaw),
                    captured_at: now()
                };
                // Keep old in-memory property access working while the persisted
                // and public payload contract uses snake_case fields.
                Object.defineProperties(event, {
                    dataRaw: { enumerable: Boolean(globalThis.__AI_CHAT_MEMORY_TEST_MODE__), get: () => event.data_raw },
                    dataJson: { enumerable: Boolean(globalThis.__AI_CHAT_MEMORY_TEST_MODE__), get: () => event.data_json },
                    capturedAt: { enumerable: Boolean(globalThis.__AI_CHAT_MEMORY_TEST_MODE__), get: () => event.captured_at }
                });
                events.push(event);
                eventName = 'message';
                dataLines = [];
            };
            for (const line of String(text || '').replace(/\r\n?/g, '\n').split('\n')) {
                if (line === '') { flush(); continue; }
                if (line.startsWith(':')) continue;
                const separator = line.indexOf(':');
                const field = separator < 0 ? line : line.slice(0, separator);
                let value = separator < 0 ? '' : line.slice(separator + 1);
                if (value.startsWith(' ')) value = value.slice(1);
                if (field === 'event') eventName = value || 'message';
                else if (field === 'data') dataLines.push(value);
            }
            flush();
            return events;
        }
    });

    const DeepSeekExchangeClassifier = Object.freeze({
        classify(value) {
            let url;
            try { url = new URL(String(value || ''), 'https://chat.deepseek.com'); } catch { return 'other_api'; }
            const path = url.pathname;
            if (path === '/api/v0/client/settings') {
                return url.searchParams.get('scope') === 'model' ? 'model_settings' : 'client_settings';
            }
            if (path.includes('/chat/history_messages')) return 'history';
            if (path.includes('/chat/completion')) return 'completion';
            if (path.includes('/file/upload_file')) return 'file_upload';
            if (/\/file\/(?:status|fetch|list|download)/.test(path)) return 'file_status';
            if (path.includes('/chat_session/fetch_page')) return 'session_page';
            if (/\/(?:search|tool|browse|web_search)(?:\/|$)/.test(path)) return 'search_or_tool';
            return 'other_api';
        },
        sessionId(exchange, fallbackPath = '') {
            if (exchange?.sessionId) return String(exchange.sessionId);
            const requestBody = typeof exchange?.request?.body === 'string'
                ? JsonTools.safeParse(exchange.request.body)
                : exchange?.request?.body;
            const bodyId = requestBody?.chat_session_id || requestBody?.chatSessionId;
            if (bodyId) return String(bodyId);
            try {
                const url = new URL(String(exchange?.request?.url || ''), 'https://chat.deepseek.com');
                const queryId = url.searchParams.get('chat_session_id');
                if (queryId) return queryId;
            } catch {}
            const responseBody = exchange?.response?.body;
            const responseId = responseBody?.data?.biz_data?.chat_session?.id
                || responseBody?.data?.biz_data?.chat_session_id
                || responseBody?.chat_session_id;
            if (responseId) return String(responseId);
            const findNestedSessionId = (value, seen = new WeakSet()) => {
                if (!value || typeof value !== 'object' || seen.has(value)) return null;
                seen.add(value);
                if (value.chat_session_id || value.chatSessionId) return value.chat_session_id || value.chatSessionId;
                for (const child of Object.values(value)) {
                    const found = findNestedSessionId(child, seen);
                    if (found) return found;
                }
                return null;
            };
            const sseId = (exchange?.response?.sse_events || exchange?.response?.sseEvents)
                ?.map(event => findNestedSessionId(event?.data_json || event?.dataJson))
                .find(Boolean);
            if (sseId) return String(sseId);
            return String(fallbackPath || '').match(/\/a\/chat\/s\/([^/?#]+)/)?.[1]
                || String(fallbackPath || '').match(/\/s\/([^/?#]+)/)?.[1]
                || null;
        }
    });

    function valueShape(value, depth = 0) {
        if (value === null) return 'null';
        if (Array.isArray(value)) return depth >= 6 ? 'array' : value.length ? [valueShape(value[0], depth + 1)] : [];
        if (typeof value !== 'object') return typeof value;
        if (depth >= 6) return 'object';
        return Object.fromEntries(Object.entries(value).map(([name, item]) => [name, valueShape(item, depth + 1)]));
    }

    class CaptureStore {
        constructor(options = {}) {
            this.getValue = options.getValue || ((key, fallback) => GM_getValue(key, fallback));
            this.setValue = options.setValue || ((key, value) => GM_setValue(key, value));
            this.now = options.now || (() => new Date().toISOString());
            this.config = { ...RuntimeConfig, ...(options.config || {}) };
            this.pendingWrite = Promise.resolve();
            this._cachedState = null;
        }

        _emptyState() {
            return {
                schema_version: this.config.captureSchemaVersion,
                client: { model_settings: null, client_settings: {}, last_protocol_headers: {} },
                sessions: {},
                unassigned: []
            };
        }

        _load() {
            if (this._cachedState) return this._cachedState;
            const stored = this.getValue(this.config.captureStorageKey, null);
            if (!stored) {
                this._cachedState = this._emptyState();
                return this._cachedState;
            }
            if (stored.schema_version !== this.config.captureSchemaVersion) {
                try {
                    this.setValue(this.config.captureStorageKey + '_backup_' + Date.now(), stored);
                } catch (e) {
                    console.warn('CaptureStore: failed to backup legacy schema state', e);
                }
                this._cachedState = this._emptyState();
                return this._cachedState;
            }
            this._cachedState = stored;
            return this._cachedState;
        }

        _persistState(state) {
            try {
                this.setValue(this.config.captureStorageKey, state);
            } catch (err) {
                console.warn('CaptureStore: setValue failed, attempting quota recovery prune', err);
                if (state.sessions && typeof state.sessions === 'object') {
                    const sessionIds = Object.keys(state.sessions);
                    if (sessionIds.length > 2) {
                        sessionIds.sort((a, b) => {
                            const tA = state.sessions[a]?.updated_at || '';
                            const tB = state.sessions[b]?.updated_at || '';
                            return tA.localeCompare(tB);
                        });
                        const toRemove = sessionIds.slice(0, Math.ceil(sessionIds.length / 2));
                        for (const id of toRemove) delete state.sessions[id];
                        try {
                            this.setValue(this.config.captureStorageKey, state);
                            return;
                        } catch (retryErr) {
                            console.error('CaptureStore: setValue retry after prune failed', retryErr);
                            throw retryErr;
                        }
                    }
                }
                throw err;
            }
        }

        _session(state, sessionId) {
            if (!sessionId || sessionId === '__proto__' || sessionId === 'constructor' || sessionId === 'prototype') {
                return {
                    latest_native_history: null,
                    latest_compatibility_history: null,
                    completion_exchanges: [],
                    file_exchanges: [],
                    other_exchanges: [],
                    updated_at: null
                };
            }
            return state.sessions[sessionId] ||= {
                latest_native_history: null,
                latest_compatibility_history: null,
                completion_exchanges: [],
                file_exchanges: [],
                other_exchanges: [],
                updated_at: null
            };
        }

        _truncateResponse(exchange) {
            const response = exchange?.response;
            if (!response) return exchange;
            const body = response.body;
            const events = response.sse_events || response.sseEvents;
            if (events !== undefined) {
                const byteLength = JsonTools.byteLength(events);
                response.byte_length = Math.max(Number(response.byte_length) || 0, byteLength);
                if (byteLength > this.config.maxResponseBytes) {
                    response.sse_events_shape = valueShape(events);
                    response.sse_events_summary = {
                        count: Array.isArray(events) ? events.length : 0,
                        event_names: Array.isArray(events) ? events.map(event => String(event?.event || 'message')) : [],
                        json_events: Array.isArray(events)
                            ? events.filter(event => event?.data_json !== null && event?.data_json !== undefined).length
                            : 0
                    };
                    response.truncated = true;
                    delete response.sse_events;
                    delete response.sseEvents;
                }
                return exchange;
            }
            if (body === undefined) return exchange;
            const byteLength = JsonTools.byteLength(body);
            response.byte_length = Math.max(Number(response.byte_length) || 0, byteLength);
            if (byteLength <= this.config.maxResponseBytes) return exchange;
            response.body_shape = valueShape(body);
            response.truncated = true;
            delete response.body;
            return exchange;
        }

        _pushBounded(target, value, limit) {
            target.push(value);
            if (target.length > limit) target.splice(0, target.length - limit);
        }

        _isCompatibilityHistory(exchange) {
            const messages = exchange?.response?.body?.data?.biz_data?.chat_messages;
            return Array.isArray(messages)
                && messages.length > 0
                && !messages.some(message => Array.isArray(message?.fragments));
        }

        _storeInSession(session, exchange, capturedAt) {
            if (exchange.kind === 'history') {
                const key = this._isCompatibilityHistory(exchange)
                    ? 'latest_compatibility_history'
                    : 'latest_native_history';
                session[key] = exchange;
            } else if (exchange.kind === 'completion') {
                this._pushBounded(session.completion_exchanges, exchange, this.config.maxCompletionExchanges);
            } else if (exchange.kind === 'file_upload' || exchange.kind === 'file_status') {
                this._pushBounded(session.file_exchanges, exchange, this.config.maxFileExchanges);
            } else {
                this._pushBounded(session.other_exchanges, exchange, this.config.maxOtherExchanges);
            }
            session.updated_at = capturedAt;
        }

        _migrateUnassigned(state, sessionId, fallbackPath, capturedAt) {
            if (!fallbackPath || !state.unassigned.length) return;
            const remaining = [];
            const session = this._session(state, sessionId);
            for (const exchange of state.unassigned) {
                const resolvedId = DeepSeekExchangeClassifier.sessionId(exchange, fallbackPath);
                if (resolvedId === sessionId) {
                    exchange.session_id = sessionId;
                    this._storeInSession(session, exchange, capturedAt);
                } else remaining.push(exchange);
            }
            state.unassigned = remaining;
        }

        _enforceSessionBudget(session) {
            let currentBytes = JsonTools.byteLength(session);
            if (currentBytes <= this.config.maxSessionBytes) return;

            const prune = (list, minRemaining = 0) => {
                while (list.length > minRemaining && currentBytes > this.config.maxSessionBytes) {
                    const removed = list.shift();
                    currentBytes -= JsonTools.byteLength(removed);
                }
            };

            prune(session.other_exchanges, 0);
            if (currentBytes > this.config.maxSessionBytes) prune(session.completion_exchanges, 1);
            if (currentBytes > this.config.maxSessionBytes) prune(session.file_exchanges, 1);
            if (currentBytes > this.config.maxSessionBytes) prune(session.completion_exchanges, 0);
            if (currentBytes > this.config.maxSessionBytes) prune(session.file_exchanges, 0);
        }

        record(exchange, fallbackPath = '') {
            const operation = async () => {
                const state = this._load();
                const capturedAt = this.now();
                const copy = this._truncateResponse(CaptureRedactor.redactExchange(JsonTools.clone(exchange)));
                copy.kind ||= DeepSeekExchangeClassifier.classify(copy?.request?.url);
                copy.session_id = DeepSeekExchangeClassifier.sessionId(copy, fallbackPath);
                copy.finished_at ||= capturedAt;
                state.client.last_protocol_headers = {
                    request: copy.request?.headers || {},
                    response: copy.response?.headers || {},
                    captured_at: capturedAt
                };

                if (copy.kind === 'model_settings') {
                    state.client.model_settings = copy;
                } else if (copy.kind === 'client_settings') {
                    let scope = 'unknown';
                    try { scope = new URL(copy.request.url).searchParams.get('scope') || 'unknown'; } catch {}
                    state.client.client_settings[scope] = copy;
                } else if (!copy.session_id) {
                    this._pushBounded(state.unassigned, copy, this.config.maxUnassignedExchanges);
                } else {
                    const session = this._session(state, copy.session_id);
                    this._migrateUnassigned(state, copy.session_id, fallbackPath, capturedAt);
                    this._storeInSession(session, copy, capturedAt);
                    this._enforceSessionBudget(session);
                }
                this._persistState(state);
                return copy;
            };
            this.pendingWrite = this.pendingWrite.then(operation, operation);
            return this.pendingWrite;
        }

        flush() {
            return this.pendingWrite;
        }

        exportSession(sessionId) {
            const state = this._load();
            const session = state.sessions[sessionId] || this._session({ sessions: {} }, sessionId);
            return JsonTools.clone({
                schema_version: state.schema_version,
                exported_at: this.now(),
                client: state.client,
                session
            });
        }
    }

    async function attachDeepSeekCapture(conversation, sessionId, captureStore, references) {
        await captureStore.flush();
        conversation._references = references;
        conversation._web_capture = captureStore.exportSession(sessionId);
        return conversation;
    }

    class NetworkCapture {
        constructor(options) {
            this.window = options.window;
            this.store = options.store;
            this.getPath = options.getPath || (() => '');
            this.now = options.now || (() => new Date().toISOString());
            this.onToken = options.onToken || (() => {});
            this.onPayload = options.onPayload || (() => {});
            this.installed = false;
        }

        static headersObject(headers) {
            if (!headers) return {};
            if (typeof headers.entries === 'function') return Object.fromEntries([...headers.entries()]);
            if (Array.isArray(headers)) return Object.fromEntries(headers);
            return { ...headers };
        }

        static serializeRequestBody(body) {
            if (body === undefined || body === null) return body ?? null;
            if (typeof body === 'string') return JsonTools.safeParse(body) ?? body;
            if (typeof body.entries === 'function' && typeof body.append === 'function') {
                const result = Object.create(null);
                for (const [name, value] of body.entries()) {
                    if (name === '__proto__' || name === 'constructor' || name === 'prototype') continue;
                    const serialized = value && typeof value === 'object'
                        && typeof value.name === 'string'
                        && typeof value.size === 'number'
                        ? { kind: 'file', name: value.name, type: String(value.type || ''), size: value.size }
                        : String(value);
                    if (Object.prototype.hasOwnProperty.call(result, name)) {
                        result[name] = Array.isArray(result[name]) ? [...result[name], serialized] : [result[name], serialized];
                    } else result[name] = serialized;
                }
                return { ...result };
            }
            if (typeof body === 'object') return JsonTools.clone(body);
            return String(body);
        }

        _isCapturable(url) {
            try {
                const parsed = new URL(String(url), 'https://chat.deepseek.com');
                return parsed.hostname === 'chat.deepseek.com' && parsed.pathname.startsWith('/api/v0/');
            } catch { return false; }
        }

        _isAllowedTokenUrl(url) {
            try {
                const parsed = new URL(String(url), 'https://chat.deepseek.com');
                const host = parsed.hostname.toLowerCase();
                return host === 'chat.deepseek.com'
                    || host === 'kimi.com'
                    || host === 'www.kimi.com'
                    || host.endsWith('.kimi.com')
                    || host.endsWith('.deepseek.com');
            } catch {
                return false;
            }
        }

        _captureToken(url, headers) {
            if (!this._isAllowedTokenUrl(url)) return;
            const entries = NetworkCapture.headersObject(headers);
            const auth = entries.authorization || entries.Authorization;
            const match = typeof auth === 'string' ? auth.match(/^Bearer\s+(.+)$/i) : null;
            if (match?.[1]?.trim()) this.onToken(match[1].trim());
        }

        _baseExchange(source, url, method, headers, body) {
            return {
                id: `${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`,
                source,
                kind: DeepSeekExchangeClassifier.classify(url),
                session_id: null,
                started_at: this.now(),
                finished_at: null,
                request: {
                    method: String(method || 'GET').toUpperCase(),
                    url: String(url),
                    headers: NetworkCapture.headersObject(headers),
                    body: NetworkCapture.serializeRequestBody(body)
                },
                response: null,
                error: null
            };
        }

        _finalize(exchange) {
            exchange.finished_at ||= this.now();
            exchange.session_id = DeepSeekExchangeClassifier.sessionId(exchange, this.getPath());
            const redacted = CaptureRedactor.redactExchange(exchange);
            try { this.onPayload(redacted.response?.body ?? redacted.response?.sse_events ?? null); } catch {}
            return this.store.record(redacted, this.getPath());
        }

        async _captureFetchResponse(exchange, response) {
            try {
                const clone = response.clone();
                const headers = NetworkCapture.headersObject(clone.headers);
                const contentType = String(headers['content-type'] || headers['Content-Type'] || '').toLowerCase();
                const text = await clone.text();
                const responseData = {
                    status: Number(clone.status || 0),
                    headers,
                    byte_length: JsonTools.byteLength(text),
                    truncated: false
                };
                if (contentType.includes('text/event-stream')) {
                    responseData.format = 'sse';
                    responseData.sse_events = SseParser.parse(text, this.now);
                } else if (contentType.includes('json')) {
                    responseData.format = 'json';
                    responseData.body = JsonTools.safeParse(text) ?? text;
                } else {
                    responseData.format = 'text';
                    responseData.body = text;
                }
                exchange.response = responseData;
            } catch (error) {
                exchange.response = { format: 'unavailable', status: Number(response?.status || 0), headers: {} };
                exchange.error = String(error?.message || error);
            }
            await this._finalize(exchange);
        }

        _installFetch() {
            const originalFetch = this.window.fetch;
            if (typeof originalFetch !== 'function' || originalFetch.__acmCaptureWrapped) return;
            const capture = this;
            function capturedFetch(input, init = {}) {
                const url = typeof input === 'string' || input instanceof URL ? String(input) : String(input?.url || input);
                const headers = init.headers || input?.headers || {};
                capture._captureToken(url, headers);
                let requestBody = init.body;
                if (!requestBody && typeof Request !== 'undefined' && input instanceof Request) {
                    try {
                        const cloned = input.clone();
                        cloned.text().then(text => {
                            if (exchange?.request) {
                                exchange.request.data_raw = text;
                                exchange.request.data_json = JsonTools.safeParse(text);
                                const sid = DeepSeekExchangeClassifier.sessionId(exchange, capture.getPath());
                                if (sid) exchange.session_id = sid;
                            }
                        }).catch(() => {});
                    } catch {}
                }
                const exchange = capture._isCapturable(url)
                    ? capture._baseExchange('fetch', url, init.method || input?.method || 'GET', headers, requestBody)
                    : null;
                const result = originalFetch.apply(this, arguments);
                if (exchange) Promise.resolve(result).then(response => capture._captureFetchResponse(exchange, response)).catch(error => {
                    exchange.error = String(error?.message || error);
                    void capture._finalize(exchange);
                });
                return result;
            }
            capturedFetch.__acmCaptureWrapped = true;
            capturedFetch.__acmOriginal = originalFetch;
            this.window.fetch = capturedFetch;
        }

        _installXhr() {
            const XHR = this.window.XMLHttpRequest;
            if (!XHR?.prototype || XHR.prototype.__acmCaptureWrapped) return;
            const capture = this;
            const originalOpen = XHR.prototype.open;
            const originalSetHeader = XHR.prototype.setRequestHeader;
            const originalSend = XHR.prototype.send;
            const metadataKey = '__acmCaptureMetadata';

            XHR.prototype.open = function(method, url) {
                this[metadataKey] = { method, url: String(url), headers: {} };
                return originalOpen.apply(this, arguments);
            };
            XHR.prototype.setRequestHeader = function(name, value) {
                if (this[metadataKey]) {
                    this[metadataKey].headers[name] = value;
                    capture._captureToken(this[metadataKey].url, { [name]: value });
                }
                return originalSetHeader.apply(this, arguments);
            };
            XHR.prototype.send = function(body) {
                const metadata = this[metadataKey];
                if (metadata && capture._isCapturable(metadata.url)) {
                    const exchange = capture._baseExchange('xhr', metadata.url, metadata.method, metadata.headers, body);
                    this.addEventListener('loadend', function() {
                        try {
                            const headers = {};
                            for (const line of String(this.getAllResponseHeaders?.() || '').split(/\r?\n/)) {
                                const separator = line.indexOf(':');
                                if (separator > 0) headers[line.slice(0, separator).trim().toLowerCase()] = line.slice(separator + 1).trim();
                            }
                            const contentType = String(headers['content-type'] || '').toLowerCase();
                            const text = typeof this.responseText === 'string' ? this.responseText : '';
                            exchange.response = {
                                status: Number(this.status || 0),
                                headers,
                                format: contentType.includes('text/event-stream') ? 'sse' : contentType.includes('json') ? 'json' : 'text',
                                byte_length: JsonTools.byteLength(text),
                                truncated: false
                            };
                            if (exchange.response.format === 'sse') exchange.response.sse_events = SseParser.parse(text, capture.now);
                            else exchange.response.body = exchange.response.format === 'json' ? (JsonTools.safeParse(text) ?? text) : text;
                        } catch (error) {
                            exchange.response = { format: 'unavailable', status: Number(this.status || 0), headers: {} };
                            exchange.error = String(error?.message || error);
                        }
                        void capture._finalize(exchange);
                    });
                }
                return originalSend.apply(this, arguments);
            };
            XHR.prototype.__acmCaptureWrapped = true;
        }

        install() {
            if (this.installed) return;
            this.installed = true;
            this._installFetch();
            this._installXhr();
        }
    }

    // ===== 配置 =====
    const DEFAULT_BRIDGE_URL = RuntimeConfig.defaultBridgeUrl;
    const BRIDGE_URL_KEY = RuntimeConfig.bridgeUrlKey;
    const BRIDGE_SECRET_KEY = RuntimeConfig.bridgeSecretKey;
    const TEST_MODE = Boolean(globalThis.__AI_CHAT_MEMORY_TEST_MODE__);
    const PLATFORM = location.hostname.includes('deepseek') ? 'deepseek'
                   : location.hostname.includes('doubao') ? 'doubao'
                   : location.hostname.includes('kimi') ? 'kimi' : null;

    const defaultSleep = (ms, signal) => new Promise((resolve, reject) => {
        if (signal?.aborted) return reject(new Error('Aborted'));
        const timer = setTimeout(resolve, ms);
        signal?.addEventListener?.('abort', () => {
            clearTimeout(timer);
            reject(new Error('Aborted'));
        }, { once: true });
    });

    class CredentialStore {
        constructor(options = {}) {
            const storage = options.storage || {};
            this.getValue = options.getValue || options.gmGet || storage.get || ((key, fallback) => GM_getValue(key, fallback));
            this.setValue = options.setValue || options.gmSet || storage.set || ((key, value) => GM_setValue(key, value));
            this.deleteValue = options.deleteValue || options.gmDelete || storage.delete || (key => GM_deleteValue(key));
            this.now = options.now || (() => Date.now());
            this.sleep = options.sleep || defaultSleep;
            this.tokenTtlMs = Number(options.tokenTtlMs ?? RuntimeConfig.tokenTtlMs);
            this.waitTimeoutMs = Number(options.waitTimeoutMs ?? options.waitMs ?? 5000);
            this.pollIntervalMs = Number(options.pollIntervalMs ?? options.pollMs ?? 100);
            this.tokenProvider = options.tokenProvider || options.loadToken || null;
        }

        capturedAtKey(tokenKey) {
            return `${tokenKey}_captured_at`;
        }

        getToken(tokenKey) {
            const token = String(this.getValue(tokenKey, '') || '').trim();
            const capturedAt = Number(this.getValue(this.capturedAtKey(tokenKey), 0));
            if (!token) return null;
            if (!capturedAt || this.now() - capturedAt > this.tokenTtlMs) {
                this.clearToken(tokenKey);
                return null;
            }
            return token;
        }

        saveToken(tokenKey, token, label = tokenKey) {
            const value = String(token || '').trim();
            if (!value || this.getValue(tokenKey, '') === value) return value || null;
            this.setValue(tokenKey, value);
            this.setValue(this.capturedAtKey(tokenKey), this.now());
            try { console.log(`✅ 已捕获${label}令牌`); } catch {}
            return value;
        }

        clearToken(tokenKey) {
            this.deleteValue(tokenKey);
            this.deleteValue(this.capturedAtKey(tokenKey));
        }

        async waitForToken(tokenKey, options = {}) {
            const timeoutMs = Math.max(0, Number(options.timeoutMs ?? this.waitTimeoutMs));
            const intervalMs = Math.max(1, Number(options.intervalMs ?? this.pollIntervalMs));
            const startedAt = Number(this.now());
            const startedRealAt = Date.now();
            while (true) {
                if (typeof this.tokenProvider === 'function') {
                    const provided = await this.tokenProvider(tokenKey);
                    if (provided) return String(provided);
                }
                const token = this.getToken(tokenKey);
                if (token) return token;
                if (options.signal?.aborted) return null;
                const clockElapsed = Number(this.now()) - startedAt;
                const elapsed = Math.max(Number.isFinite(clockElapsed) ? clockElapsed : 0, Date.now() - startedRealAt);
                if (timeoutMs === 0 || elapsed >= timeoutMs) return null;
                await this.sleep(Math.min(intervalMs, timeoutMs));
            }
        }

        getSecret(secretKey = RuntimeConfig.bridgeSecretKey) {
            return String(this.getValue(secretKey, '') || '').trim();
        }

        setSecret(secretKey, secret) {
            const value = String(secret || '').trim();
            if (value) this.setValue(secretKey, value);
            else this.deleteValue(secretKey);
            return value;
        }

        clearSecret(secretKey = RuntimeConfig.bridgeSecretKey) {
            this.deleteValue(secretKey);
        }

        get(tokenKey) {
            return this.getToken(tokenKey);
        }

        set(tokenKey, token, label = tokenKey) {
            return this.saveToken(tokenKey, token, label);
        }

        clear(tokenKey) {
            return this.clearToken(tokenKey);
        }

        wait(tokenKey, options = {}) {
            return this.waitForToken(tokenKey, options);
        }
    }

    class BridgeClient {
        constructor(options = {}) {
            this.fetchImpl = options.fetch || options.fetchImpl || options.request || globalThis.fetch;
            this.secretProvider = options.getSecret || null;
            this.credentials = options.credentials || new CredentialStore({
                getValue: options.getValue,
                setValue: options.setValue,
                deleteValue: options.deleteValue,
                storage: options.storage
            });
            const storage = options.storage || {};
            this.getValue = options.getValue || options.gmGet || storage.get || ((key, fallback) => GM_getValue(key, fallback));
            this.setValue = options.setValue || options.gmSet || storage.set || ((key, value) => GM_setValue(key, value));
            this.deleteValue = options.deleteValue || options.gmDelete || storage.delete || (key => GM_deleteValue(key));
            this.defaultUrl = options.defaultUrl || RuntimeConfig.defaultBridgeUrl;
            this.urlKey = options.urlKey || RuntimeConfig.bridgeUrlKey;
            this.secretKey = options.secretKey || RuntimeConfig.bridgeSecretKey;
            this.normalizeUrl = options.normalizeUrl || BridgeClient.normalizeUrl;
            this.configuredUrl = this._readConfiguredUrl();
            this.activeUrl = this.configuredUrl;
            this.baseUrl = this.activeUrl;
        }

        static normalizeUrl(value) {
            let normalized = String(value || '').trim();
            if (!normalized) throw new Error('后端地址不能为空');
            if (!/^https?:\/\//i.test(normalized)) normalized = `http://${normalized}`;
            const url = new URL(normalized);
            if (!['http:', 'https:'].includes(url.protocol)) {
                throw new Error('后端地址仅支持 http:// 或 https://');
            }
            return url.toString().replace(/\/+$/, '');
        }

        _readConfiguredUrl() {
            try { return this.normalizeUrl(this.getValue(this.urlKey, this.defaultUrl)); } catch {
                this.deleteValue(this.urlKey);
                return this.normalizeUrl(this.defaultUrl);
            }
        }

        urlCandidates() {
            const candidates = [this.configuredUrl];
            if (!/\/api\/v1$/i.test(this.configuredUrl)) candidates.push(`${this.configuredUrl}/api/v1`);
            return [...new Set(candidates)];
        }

        headers(extra = {}) {
            const headers = {};
            if (extra && typeof extra.entries === 'function') {
                for (const [name, value] of extra.entries()) headers[name] = value;
            } else Object.assign(headers, extra || {});
            const result = { 'X-AI-Chat-Memory-Client': 'userscript-v1', ...headers };
            const secret = this.secretProvider ? this.secretProvider(this.secretKey) : this.credentials.getSecret(this.secretKey);
            if (secret) result['X-AI-Chat-Memory-Secret'] = secret;
            return result;
        }

        _secretError() {
            return this.credentials.getSecret(this.secretKey)
                ? '本地服务密钥错误，请通过脚本菜单重新填写密钥'
                : '本地服务已启用密钥验证，请通过脚本菜单填写密钥';
        }

        async _responseText(response) {
            const readable = typeof response?.clone === 'function' ? response.clone() : response;
            return typeof readable?.text === 'function' ? readable.text() : '';
        }

        _url(path, base = this.activeUrl) {
            const value = String(path || '');
            return /^https?:\/\//i.test(value) ? value : `${base}${value.startsWith('/') ? value : `/${value}`}`;
        }

        async request(path, options = {}) {
            if (typeof this.fetchImpl !== 'function') throw new Error('本地服务 fetch 不可用');
            const response = await this.fetchImpl(this._url(path), {
                ...options,
                headers: this.headers(options.headers)
            });
            if (response?.status === 403) {
                const reason = await this._responseText(response);
                if (reason === 'invalid_secret') throw new Error(this._secretError());
                throw new Error(`本地服务拒绝请求: ${reason || '安全策略不匹配'}`);
            }
            return response;
        }

        fetch(path, options = {}) {
            return this.request(path, options);
        }

        async checkServer() {
            let lastError = null;
            for (const candidate of this.urlCandidates()) {
                try {
                    const response = await this.fetchImpl(`${candidate}/health`, { headers: this.headers() });
                    if (response.status === 403) {
                        const reason = await this._responseText(response);
                        if (reason === 'invalid_secret') return { state: 'secret', message: this._secretError() };
                        return { state: 'rejected', message: `本地服务拒绝请求: ${reason || '安全策略不匹配'}` };
                    }
                    if (response.ok) {
                        this.activeUrl = candidate;
                        this.baseUrl = candidate;
                        return { state: 'connected', url: candidate };
                    }
                    lastError = `HTTP ${response.status}`;
                } catch (error) {
                    lastError = error?.message || String(error);
                }
            }
            return { state: 'unreachable', message: lastError || '无法连接本地服务' };
        }

        check() {
            return this.checkServer();
        }

        health() {
            return this.checkServer();
        }

        async json(path, options = {}) {
            const response = await this.request(path, options);
            if (!response.ok) throw new Error(`请求失败 ${response.status}: ${await this._responseText(response)}`);
            return response.json();
        }

        setUrl(value) {
            const normalized = this.normalizeUrl(value);
            this.setValue(this.urlKey, normalized);
            this.configuredUrl = normalized;
            this.activeUrl = normalized;
            this.baseUrl = normalized;
            return normalized;
        }

        resetUrl() {
            this.deleteValue(this.urlKey);
            this.configuredUrl = this.normalizeUrl(this.defaultUrl);
            this.activeUrl = this.configuredUrl;
            this.baseUrl = this.activeUrl;
        }
    }

    // ===== 适配器基类 =====
    class BaseAdapter {
        platform = '';
        needsToken = false;
        tokenKey = '';

        constructor(options = {}) {
            this.fetchImpl = options.fetch || options.fetchImpl || globalThis.fetch;
            this.request = options.request || options.requestJson || null;
            this.window = options.window || globalThis;
            this.xhrFactory = options.xhrFactory || (() => new this.window.XMLHttpRequest());
            this.credentials = options.credentials || new CredentialStore();
            this.location = options.location || globalThis.location || { pathname: '' };
            this.sleep = options.sleep || defaultSleep;
            this.tokenWaitOptions = options.tokenWaitOptions || {};
            this.randomUUID = options.randomUUID || (() => globalThis.crypto?.randomUUID?.()
                || `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`);
            this.tokenProvider = options.tokenProvider || options.getToken || null;
        }

        async getToken() { return null; }
        async fetchAllSessions() { return []; }
        async fetchConversation(id) { return null; }
        getCurrentSessionId() { return null; }

        listSessions() {
            return this.fetchAllSessions();
        }

        getConversation(id) {
            return this.fetchConversation(id);
        }

        async _fetchJson(url, options = {}) {
            if (this.request) {
                const value = await this.request(url, options);
                return value && typeof value.json === 'function' ? value.json() : value;
            }
            const response = await this.fetchImpl(url, options);
            return response.json();
        }
    }

    // ===== DeepSeek 适配器 =====
    class DeepSeekAdapter extends BaseAdapter {
        platform = 'deepseek';
        needsToken = true;
        tokenKey = 'ds_token';

        constructor(options = {}) {
            super(options);
            this.referenceCacheKey = RuntimeConfig.referenceStorageKey;
            this.captureStore = options.captureStore || null;
            this.getValue = options.getValue || ((key, fallback) => GM_getValue(key, fallback));
            this.setValue = options.setValue || ((key, value) => GM_setValue(key, value));
            this.autoWaitForToken = options.autoWaitForToken !== false;
        }

        _extractReferences(value) {
            const references = [];
            const seen = new Set();
            const visit = item => {
                if (Array.isArray(item)) return item.forEach(visit);
                if (!item || typeof item !== 'object' || seen.has(item)) return;
                seen.add(item);
                const url = typeof item.url === 'string' ? item.url : typeof item.link === 'string' ? item.link : '';
                const citeIndex = Number(item.cite_index ?? item.citeIndex ?? item.index);
                if (/^https?:\/\//i.test(url) && Number.isInteger(citeIndex) && citeIndex >= 0) {
                    references.push({
                        cite_index: citeIndex,
                        url,
                        title: String(item.title ?? item.name ?? ''),
                        snippet: String(item.snippet ?? item.summary ?? item.description ?? '')
                    });
                }
                Object.values(item).forEach(visit);
            };
            visit(value);
            return references;
        }

        _cacheReferences(value) {
            const sessionId = this.getCurrentSessionId();
            const found = this._extractReferences(value);
            if (!sessionId || !found.length) return;
            const cache = this.getValue(this.referenceCacheKey, {}) || {};
            const merged = new Map((cache[sessionId] || []).map(item => [item.cite_index, item]));
            found.forEach(item => merged.set(item.cite_index, item));
            cache[sessionId] = [...merged.values()].sort((a, b) => a.cite_index - b.cite_index);
            this.setValue(this.referenceCacheKey, cache);
            console.log(`🔗 DeepSeek: 已缓存 ${cache[sessionId].length} 条网页引用`);
        }

        _referencesForSession(sessionId) {
            return (this.getValue(this.referenceCacheKey, {}) || {})[sessionId] || [];
        }

        async getToken(options = {}) {
            const wait = options.wait ?? this.autoWaitForToken;
            if (wait) return this.credentials.waitForToken(this.tokenKey, { ...this.tokenWaitOptions, ...options });
            return this.credentials.getToken(this.tokenKey);
        }

        async _xhr(url, retry = 0, signal = null) {
            if (signal?.aborted) throw new Error('Aborted');
            const token = await this.getToken({ wait: true, signal });
            if (this.needsToken && !token) throw new Error('Token 未就绪');
            if (this.request) {
                const value = await this.request(url, {
                    method: 'GET',
                    headers: {
                        Authorization: `Bearer ${token}`,
                        'x-client-version': '1.7.0',
                        'x-app-version': '20241129.1',
                        'x-client-locale': 'zh_CN',
                        'x-client-platform': 'web',
                        'x-client-timezone-offset': '28800'
                    },
                    signal
                });
                return value && typeof value.json === 'function' ? value.json() : value;
            }
            return new Promise((resolve, reject) => {
                if (signal?.aborted) return reject(new Error('Aborted'));
                const xhr = this.xhrFactory();
                xhr.open('GET', url);
                xhr.withCredentials = true;
                xhr.setRequestHeader('Authorization', `Bearer ${token}`);
                xhr.setRequestHeader('x-client-version', '1.7.0');
                xhr.setRequestHeader('x-app-version', '20241129.1');
                xhr.setRequestHeader('x-client-locale', 'zh_CN');
                xhr.setRequestHeader('x-client-platform', 'web');
                xhr.setRequestHeader('x-client-timezone-offset', '28800');
                const onAbort = () => {
                    try { xhr.abort(); } catch {}
                    reject(new Error('Aborted'));
                };
                signal?.addEventListener?.('abort', onAbort, { once: true });
                xhr.onload = () => {
                    signal?.removeEventListener?.('abort', onAbort);
                    if (xhr.status === 429 && retry < 3) {
                        const wait = (retry + 1) * 15000;
                        console.warn(`⚠️ 429限流，${wait/1000}s 后重试 (${retry+1}/3)`);
                        this.sleep(wait, signal).then(() => this._xhr(url, retry + 1, signal).then(resolve, reject), reject);
                        return;
                    }

                    if (xhr.status < 200 || xhr.status >= 300) {
                        reject(new Error(`DeepSeek 请求失败 ${xhr.status}: ${this._formatError(xhr.responseText)}`));
                        return;
                    }

                    try {
                        resolve(JSON.parse(xhr.responseText));
                    } catch (e) {
                        reject(new Error(`DeepSeek 响应解析失败: ${e.message}`));
                    }
                };
                xhr.onerror = () => {
                    signal?.removeEventListener?.('abort', onAbort);
                    reject(new Error(`XHR error: ${xhr.status}`));
                };
                xhr.send();
            });
        }

        _formatError(text) {
            if (!text) return '无响应内容';
            try {
                const json = JSON.parse(text);
                return json.message || json.msg || json.error || json.detail || JSON.stringify(json).slice(0, 160);
            } catch {
                return String(text).slice(0, 160);
            }
        }

        _extractSessionsPage(json, label = '会话列表') {
            const bizData = json?.data?.biz_data;
            if (!bizData || !Array.isArray(bizData.chat_sessions)) {
                const detail = json?.message || json?.msg || json?.error || json?.detail || json?.code;
                throw new Error(`${label}响应异常${detail ? `: ${detail}` : ''}`);
            }
            return {
                sessions: bizData.chat_sessions,
                hasMore: Boolean(bizData.has_more)
            };
        }

        async fetchAllSessions() {
            const token = await this.getToken({ wait: true });
            if (!token) throw new Error('Token 未就绪');
            const allSessions = [];
            const MAX_PAGES = 500;
            let pageCount = 0;

            let json = await this.fetchSessionPage();
            let { sessions, hasMore } = this._extractSessionsPage(json);
            allSessions.push(...sessions);
            console.log(`📋 首页 ${sessions.length} 条, hasMore=${hasMore}`);

            while (hasMore && sessions.length > 0 && pageCount < MAX_PAGES) {
                pageCount++;
                const cursor = sessions[sessions.length - 1]?.updated_at;
                if (!cursor) break;
                json = await this.fetchSessionPage(cursor);
                const next = this._extractSessionsPage(json);
                if (!next.sessions || !next.sessions.length) break;
                allSessions.push(...next.sessions);
                hasMore = next.hasMore;
                sessions = next.sessions;
                console.log(`📋 本页 ${sessions.length} 条, 累计 ${allSessions.length}, hasMore=${hasMore}`);
            }

            console.log(`📋 会话获取完成，共 ${allSessions.length} 个`);
            return allSessions;
        }

        async fetchSessionPage(cursor = null) {
            const query = 'https://chat.deepseek.com/api/v0/chat_session/fetch_page?lte_cursor.pinned=false'
                + (cursor ? `&lte_cursor.updated_at=${encodeURIComponent(cursor)}` : '');
            return this._xhr(query);
        }

        async fetchConversation(id) {
            const conversation = await this._xhr(`https://chat.deepseek.com/api/v0/chat/history_messages?chat_session_id=${encodeURIComponent(id)}`);
            if (!this.captureStore) {
                conversation._references = this._referencesForSession(id);
                return conversation;
            }
            return attachDeepSeekCapture(
                conversation,
                id,
                this.captureStore,
                this._referencesForSession(id)
            );
        }

        async createOfficialExportTask() {
            return this._xhr('https://chat.deepseek.com/api/v0/export_all');
        }

        async fetchOfficialExportStatus() {
            return this._xhr('https://chat.deepseek.com/api/v0/download_export_history');
        }

        extractOfficialZipUrl(json) {
            const seen = new Set();
            const findZip = (value) => {
                if (typeof value === 'string') {
                    return /^https?:\/\/.+(?:\.zip(?:\?|$)|(?:\/download|\/export)(?:[/?#]|$))/.test(value) ? value : null;
                }
                if (!value || typeof value !== 'object') return null;
                if (seen.has(value)) return null;
                seen.add(value);
                for (const item of Object.values(value)) {
                    const matched = findZip(item);
                    if (matched) return matched;
                }
                return null;
            };
            return findZip(json);
        }

        describeExportStatus(json) {
            const text = JSON.stringify(json || {});
            if (/未创建|not.?created|no.?task/i.test(text)) return '官方导出任务未创建';
            if (/生成|处理中|进行中|pending|running|processing/i.test(text)) return '官方导出生成中';
            return '官方导出生成中';
        }

        getCurrentSessionId() {
            return this.location.pathname.match(/\/s\/([^/?]+)/)?.[1];
        }
    }

    // ===== 豆包适配器 =====
    class DoubaoAdapter extends BaseAdapter {
        platform = 'doubao';
        needsToken = false;
        apiParams = 'version_code=20800&language=zh&device_platform=web&aid=497858&real_aid=497858&pkg_type=release_version&device_id=0&pc_version=3.5.9&samantha_web=1&use-olympus-account=1';

        async fetchAllSessions() {
            const url = `https://www.doubao.com/im/chain/recent_conv?${this.apiParams}`;
            const options = {
                method: 'POST',
                headers: { 'content-type': 'application/json; encoding=utf-8' },
                body: JSON.stringify({
                    cmd: 3200,
                    uplink_body: { pull_recent_conv_chain_uplink_body: { limit: 100, api_version: 1, direction: 3, option: { not_need_message: true, need_complete_conversation: true } } },
                    sequence_id: this.randomUUID(),
                    channel: 2,
                    version: "1"
                })
            };
            const json = await this._fetchJson(url, options);
            return json?.downlink_body?.pull_recent_conv_chain_downlink_body?.cells || [];
        }

        async fetchConversation(id) {
            const url = `https://www.doubao.com/im/chain/single?${this.apiParams}`;
            const options = {
                method: 'POST',
                headers: { 'content-type': 'application/json; encoding=utf-8' },
                body: JSON.stringify({
                    cmd: 3100,
                    uplink_body: { pull_singe_chain_uplink_body: { conversation_id: id, conversation_type: 3, anchor_index: 9007199254740991, direction: 1, limit: 1000 } },
                    sequence_id: this.randomUUID(),
                    channel: 2,
                    version: "1"
                })
            };
            return this._fetchJson(url, options);
        }

        getCurrentSessionId() {
            return this.location.pathname.match(/\/chat\/(\d+)/)?.[1];
        }
    }

    // ===== Kimi 适配器 =====
    class KimiAdapter extends BaseAdapter {
        platform = 'kimi';
        needsToken = true;
        tokenKey = 'kimi_token';

        constructor(options = {}) {
            super(options);
            this.autoWaitForToken = options.autoWaitForToken !== false;
            if (options.autoCaptureToken !== false && options.window) this.installTokenCapture(options.window);
        }

        installTokenCapture(windowObject = globalThis) {
            const originalFetch = windowObject.fetch;
            if (typeof originalFetch !== 'function' || originalFetch.__acmCredentialWrapped) return;
            const tokenKey = this.tokenKey;
            const credentials = this.credentials;
            windowObject.fetch = function(...args) {
                try {
                    const input = args[0];
                    const url = typeof input === 'string' || input instanceof URL ? String(input) : String(input?.url || input || '');
                    const parsed = new URL(url, 'https://www.kimi.com');
                    const host = parsed.hostname.toLowerCase();
                    if (host === 'kimi.com' || host === 'www.kimi.com' || host.endsWith('.kimi.com')) {
                        const options = args[1] || {};
                        const headers = options.headers || args[0]?.headers;
                        const auth = headers && typeof headers.get === 'function'
                            ? headers.get('authorization')
                            : headers?.authorization || headers?.Authorization;
                        const match = typeof auth === 'string' && auth.match(/^Bearer\s+(.+)$/i);
                        if (match && match[1].trim() !== '') {
                            credentials.saveToken(tokenKey, match[1], 'Kimi');
                        }
                    }
                } catch {}
                return originalFetch.apply(this, args);
            };
            windowObject.fetch.__acmCredentialWrapped = true;
            windowObject.fetch.__acmOriginal = originalFetch;
        }

        async getToken(options = {}) {
            const wait = options.wait ?? this.autoWaitForToken;
            if (wait) return this.credentials.waitForToken(this.tokenKey, { ...this.tokenWaitOptions, ...options });
            return this.credentials.getToken(this.tokenKey);
        }

        async _fetch(url, body) {
            const token = await this.getToken({ wait: true });
            if (this.needsToken && !token) throw new Error('Kimi Token 未就绪');
            const options = {
                method: 'POST',
                headers: {
                    'Content-Type': 'application/json',
                    'Authorization': `Bearer ${token}`,
                    'x-msh-platform': 'web'
                },
                credentials: 'include',
                body: JSON.stringify(body)
            };
            if (this.request) {
                const value = await this.request(url, options);
                if (value?.status === 429) throw new Error('Kimi 429 限流');
                return value && typeof value.json === 'function' ? value.json() : value;
            }
            const res = await this.fetchImpl(url, options);
            if (res.status === 429) throw new Error('Kimi 429 限流');
            return res.json();
        }

        async fetchAllSessions() {
            const token = await this.getToken();
            if (!token) throw new Error('Kimi Token 未就绪');
            const all = [];
            let pageToken = '';
            let pageCount = 0;
            const MAX_PAGES = 500;
            do {
                pageCount++;
                const body = { project_id: '', page_size: 200, query: '' };
                if (pageToken) body.page_token = pageToken;
                const json = await this._fetch('https://www.kimi.com/apiv2/kimi.chat.v1.ChatService/ListChats', body);
                const chats = json.chats || [];
                all.push(...chats);
                const nextToken = json.nextPageToken || '';
                if (!nextToken || nextToken === pageToken) break;
                pageToken = nextToken;
                console.log(`📋 Kimi: 本页 ${chats.length} 条, 累计 ${all.length}`);
            } while (pageToken && pageCount < MAX_PAGES);
            return all;
        }

        async fetchConversation(id) {
            const json = await this._fetch('https://www.kimi.com/apiv2/kimi.gateway.chat.v1.ChatService/ListMessages', { chat_id: id, page_size: 1000 });
            return json;
        }

        getCurrentSessionId() {
            return this.location.pathname.match(/\/chat\/([^/?]+)/)?.[1];
        }
    }

    class SyncCoordinator {
        constructor(options = {}) {
            this.adapter = options.adapter;
            this.platform = options.platform || this.adapter?.platform || PLATFORM;
            this.bridge = options.bridgeClient || options.bridge || null;
            this.ui = options.ui || null;
            this.fetchImpl = options.fetch || globalThis.fetch;
            this.sleep = options.sleep || (ms => new Promise(resolve => setTimeout(resolve, ms)));
            this.detailConcurrency = Math.max(1, Number(options.detailConcurrency || 4));
            this.detailDelayMs = Math.max(0, Number(options.detailDelayMs ?? 50));
            this.exportPollAttempts = Math.max(1, Number(options.exportPollAttempts || 72));
            this.exportPollDelayMs = Math.max(0, Number(options.exportPollDelayMs ?? 5000));
            this.stopped = false;
            this.abortController = null;
        }

        _status(text) {
            try { this.ui?.setStatus(text); } catch {}
        }

        _progress(current, total, text) {
            try { this.ui?.setProgress(current, total, text); } catch {}
        }

        _isStopped() {
            return this.stopped || Boolean(this.abortController?.signal?.aborted);
        }

        stop() {
            this.stopped = true;
            try { this.abortController?.abort(); } catch {}
            this._status('⏹ 已停止');
        }

        stopSync() {
            this.stop();
        }

        static toEpochSeconds(value) {
            if (value === null || value === undefined || value === '') return 0;
            const numeric = Number(value);
            if (Number.isFinite(numeric)) return Math.abs(numeric) > 1e11 ? numeric / 1000 : numeric;
            const milliseconds = Date.parse(String(value));
            return Number.isFinite(milliseconds) ? milliseconds / 1000 : 0;
        }

        toEpochSeconds(value) {
            return SyncCoordinator.toEpochSeconds(value);
        }

        async fetchSessionsIncremental(lastUpdatedAt) {
            const token = this.adapter?.needsToken ? await this.adapter.getToken({ wait: true }) : true;
            if (this.adapter?.needsToken && !token) throw new Error('Token 未就绪');
            const newSessions = [];
            const lastUpdatedSeconds = this.toEpochSeconds(lastUpdatedAt);
            let cursor = null;
            let hasMore = true;
            let pageCount = 0;
            const MAX_INCREMENTAL_PAGES = 200;

            while (hasMore && !this._isStopped() && pageCount < MAX_INCREMENTAL_PAGES) {
                pageCount++;
                const json = this.adapter.fetchSessionPage
                    ? await this.adapter.fetchSessionPage(cursor)
                    : await this.adapter._xhr(`https://chat.deepseek.com/api/v0/chat_session/fetch_page?lte_cursor.pinned=false${cursor ? `&lte_cursor.updated_at=${encodeURIComponent(cursor)}` : ''}`);
                const page = this.adapter._extractSessionsPage
                    ? this.adapter._extractSessionsPage(json, '增量会话列表')
                    : {
                        sessions: json?.sessions || json?.data?.biz_data?.chat_sessions || [],
                        hasMore: Boolean(json?.hasMore ?? json?.has_more ?? json?.data?.biz_data?.has_more)
                    };
                const sessions = page.sessions || [];
                hasMore = Boolean(page.hasMore);
                let hitOld = false;
                for (const session of sessions) {
                    const updatedSeconds = this.toEpochSeconds(session.updated_at);
                    if (session.pinned) {
                        if (updatedSeconds > lastUpdatedSeconds) newSessions.push(session);
                        continue;
                    }
                    if (lastUpdatedSeconds && updatedSeconds <= lastUpdatedSeconds) {
                        hitOld = true;
                        break;
                    }
                    newSessions.push(session);
                }
                if (hitOld || !sessions.length) break;
                const nextCursor = sessions[sessions.length - 1]?.updated_at;
                if (!nextCursor || nextCursor === cursor) break;
                cursor = nextCursor;
            }
            return newSessions;
        }

        async selectSessions(lastUpdatedAt) {
            if (!lastUpdatedAt) {
                this._status('本地为空，全量拉取...');
                return this.adapter.fetchAllSessions();
            }
            if (this.platform === 'deepseek' && (this.adapter.fetchSessionPage || this.adapter._xhr)) {
                this._status('增量拉取...');
                return this.fetchSessionsIncremental(lastUpdatedAt);
            }
            return this.adapter.fetchAllSessions();
        }

        _sessionId(session) {
            return this.platform === 'doubao' ? session?.conversation?.conversation_id : session?.id;
        }

        async _downloadOfficialZip(zipUrl) {
            if (typeof GM_xmlhttpRequest === 'function') {
                try {
                    const blob = await new Promise((resolve, reject) => {
                        const req = GM_xmlhttpRequest({
                            method: 'GET',
                            url: zipUrl,
                            responseType: 'blob',
                            onload: (res) => {
                                if (res.status >= 200 && res.status < 300) resolve(res.response);
                                else reject(new Error(`GM_xmlhttpRequest failed ${res.status}`));
                            },
                            onerror: (err) => reject(new Error(`GM_xmlhttpRequest error: ${err}`)),
                            ontimeout: () => reject(new Error('GM_xmlhttpRequest timeout'))
                        });
                        this.abortController?.signal?.addEventListener('abort', () => {
                            try { req?.abort?.(); } catch {}
                            reject(new Error('Aborted'));
                        }, { once: true });
                    });
                    if (blob && blob.size) return blob;
                } catch (e) {
                    console.warn('GM_xmlhttpRequest failed, falling back to fetch', e);
                }
            }
            const zipResponse = await this.fetchImpl(zipUrl, {
                method: 'GET', credentials: 'omit', mode: 'cors', signal: this.abortController?.signal
            });
            if (!zipResponse.ok) throw new Error(`ZIP 下载失败 ${zipResponse.status}: ${await zipResponse.text()}`);
            return zipResponse.blob();
        }

        async syncDeepSeekOfficialExport() {
            const token = await this.adapter.getToken({ wait: true });
            if (!token) throw new Error('Token 未就绪');

            this._progress(1, 4, '创建官方导出任务...');
            await this.adapter.createOfficialExportTask();
            if (this._isStopped()) return { state: 'stopped' };

            let zipUrl = null;
            for (let attempt = 1; attempt <= this.exportPollAttempts && !this._isStopped(); attempt++) {
                this._progress(2, 4, `等待官方导出 ${attempt}/${this.exportPollAttempts}`);
                const status = await this.adapter.fetchOfficialExportStatus();
                zipUrl = this.adapter.extractOfficialZipUrl(status);
                if (zipUrl) break;
                this._status(this.adapter.describeExportStatus(status));
                await this.sleep(this.exportPollDelayMs, this.abortController?.signal);
            }
            if (this._isStopped()) return { state: 'stopped' };
            if (!zipUrl) throw new Error('官方导出超时，未获取到 ZIP 下载地址');

            this._progress(3, 4, '下载官方 ZIP...');
            const zipBlob = await this._downloadOfficialZip(zipUrl);
            if (!zipBlob || !zipBlob.size) throw new Error('ZIP 下载为空');
            if (this._isStopped()) return { state: 'stopped' };

            this._progress(4, 4, '导入官方 ZIP...');
            const importResponse = await this.bridge.request('/sessions/import/deepseek-export', {
                method: 'POST',
                headers: { 'Content-Type': 'application/zip' },
                body: zipBlob,
                signal: this.abortController?.signal
            });
            if (!importResponse.ok) throw new Error(`官方 ZIP 导入失败 ${importResponse.status}: ${await importResponse.text()}`);
            const data = await importResponse.json();
            this._status(`✅ 官方导入 ${data.imported} 个, 跳过 ${data.skipped} 个`);
            return data;
        }

        async fetchDetailsAndPush(sessions) {
            if (!sessions.length) {
                this._status('✅ 无新会话需要同步');
                return { imported: 0, skipped: 0, sessions: [] };
            }
            const queue = [...sessions];
            const results = [];
            const failed = [];
            let counter = 0;
            const worker = async () => {
                while (queue.length && !this._isStopped()) {
                    const session = queue.shift();
                    const id = this._sessionId(session);
                    const number = ++counter;
                    this._progress(number, sessions.length, `获取详情 ${number}/${sessions.length}`);
                    try {
                        const conversation = await this.adapter.fetchConversation(id);
                        results.push({ ...session, _conversation: conversation });
                    } catch (err) {
                        console.warn(`会话 ${id} 获取失败:`, err);
                        failed.push({ id, session, error: String(err?.message || err) });
                    }
                    if (this.detailDelayMs) await this.sleep(this.detailDelayMs, this.abortController?.signal);
                }
            };
            await Promise.all(Array.from({ length: this.detailConcurrency }, worker));
            if (this._isStopped()) return { state: 'stopped', sessions: results };

            if (!results.length && failed.length > 0) {
                throw new Error(`全部 ${failed.length} 个会话获取详情失败: ${failed[0].error}`);
            }

            this._progress(1, 1, '推送到服务端...');
            const response = await this.bridge.request('/sessions/import', {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ platform: this.platform, sessions: results }),
                signal: this.abortController?.signal
            });
            if (!response.ok) throw new Error(`导入失败 ${response.status}: ${await response.text()}`);
            const data = await response.json();
            const failMsg = failed.length ? `, 失败 ${failed.length} 个` : '';
            this._status(`✅ 导入 ${data.imported} 个, 跳过 ${data.skipped} 个${failMsg}`);
            return { ...data, sessions: results, failed };
        }

        async sync(fullSync = false) {
            if (!this.bridge) throw new Error('BridgeClient 未配置');
            this.stopped = false;
            this.abortController = typeof AbortController === 'function' ? new AbortController() : null;
            let connection;
            try {
                connection = await this.bridge.checkServer();
            } catch (error) {
                this._status('❌ ' + (error?.message || String(error)));
                return { state: 'error', error };
            }
            if (connection.state !== 'connected') {
                this._status(`❌ ${connection.message}`);
                return { state: connection.state, connection };
            }
            if (this._isStopped()) return { state: 'stopped' };
            try { this.ui?.setSyncing(true); } catch {}
            try {
                if (fullSync && this.platform === 'deepseek') return await this.syncDeepSeekOfficialExport();
                let sessions;
                if (fullSync) {
                    this._status('全量拉取会话列表...');
                    sessions = await this.adapter.fetchAllSessions();
                    this._status(`全量获取 ${sessions.length} 个会话`);
                } else {
                    this._status('查询同步状态...');
                    const statusResponse = await this.bridge.request(`/sessions/sync-status?platform=${encodeURIComponent(this.platform)}`, {
                        signal: this.abortController?.signal
                    });
                    if (!statusResponse.ok) throw new Error(`同步状态查询失败 ${statusResponse.status}: ${await statusResponse.text()}`);
                    const status = await statusResponse.json();
                    sessions = await this.selectSessions(status.last_updated_at);
                    this._status(`需同步 ${sessions.length} 个会话`);
                }
                if (this._isStopped()) return { state: 'stopped', sessions: [] };
                return this.fetchDetailsAndPush(sessions);
            } catch (error) {
                if (this._isStopped()) return { state: 'stopped' };
                this._status('❌ ' + (error?.message || String(error)));
                return { state: 'error', error };
            } finally {
                try { this.ui?.setSyncing(false); } catch {}
                this.abortController = null;
            }
        }

        run(fullSync = false) {
            return this.sync(fullSync);
        }

        syncToServer(fullSync = false) {
            return this.sync(fullSync);
        }
    }

    class SyncPanel {
        constructor(options = {}) {
            this.document = options.document || globalThis.document || null;
            this.onSync = options.onSync || (() => {});
            this.onFullSync = options.onFullSync || (() => {});
            this.onStop = options.onStop || (() => {});
            this.onCheckServer = options.onCheckServer || null;
            this.setIntervalImpl = options.setInterval || globalThis.setInterval;
            this.clearIntervalImpl = options.clearInterval || globalThis.clearInterval;
            this.setTimeoutImpl = options.setTimeout || globalThis.setTimeout || (() => {});
            this.pollIntervalMs = Number(options.pollIntervalMs || 18000);
            this.panel = null;
            this.pollTimer = null;
            this.mounted = false;
            if (options.autoMount !== false && this.document?.createElement) this.mount();
        }

        mount() {
            if (this.mounted || !this.document?.createElement) return this;
            const panel = this.document.createElement('div');
            panel.innerHTML = `
        <div id="acm-panel" style="position:fixed;top:70px;right:70px;z-index:99999;background:#fff;border:1px solid #d0d0d0;box-shadow:0 2px 8px rgba(0,0,0,0.12);border-radius:6px;padding:14px 18px;font-family:system-ui,sans-serif;font-size:13px;color:#333;width:260px;">
            <div style="display:flex;justify-content:space-between;align-items:center;margin-bottom:10px;">
                <span style="font-weight:600;font-size:14px;">🧠 AI Chat Memory</span>
                <span id="acm-close" style="cursor:pointer;font-size:16px;color:#999;line-height:1;" title="关闭面板">✕</span>
            </div>
            <div id="acm-status-line" style="margin-bottom:8px;color:#888;">就绪</div>
            <div style="background:#eee;border-radius:3px;height:6px;margin-bottom:10px;overflow:hidden;">
                <div id="acm-bar" style="width:0%;height:100%;background:#4caf50;transition:width .3s;"></div>
            </div>
            <div style="display:flex;gap:8px;margin-bottom:8px;">
                <button id="acm-sync-btn" style="flex:1;padding:6px 0;border:none;border-radius:4px;background:#4caf50;color:#fff;cursor:pointer;font-size:13px;">开始同步</button>
                <button id="acm-full-btn" style="flex:1;padding:6px 0;border:none;border-radius:4px;background:#2196f3;color:#fff;cursor:pointer;font-size:13px;">全量同步</button>
            </div>
            <div style="font-size:12px;color:#aaa;">服务: <span id="acm-srv">检测中...</span></div>
            </div>`;
            const host = this.document.body || this.document.documentElement;
            if (!host) {
                this.document.addEventListener?.('DOMContentLoaded', () => this.mount(), { once: true });
                return this;
            }
            host.appendChild(panel);
            this.panel = panel;
            this.mounted = true;
            const syncButton = this._query('acm-sync-btn');
            const fullButton = this._query('acm-full-btn');
            syncButton && (syncButton.onclick = () => {
                if (syncButton.dataset?.syncing === '1') this.onStop();
                else this.onSync(false);
            });
            fullButton && (fullButton.onclick = () => {
                if (!fullButton.disabled) this.onFullSync(true);
            });
            const closeButton = this._query('acm-close');
            closeButton && (closeButton.onclick = () => {
                const root = this._query('acm-panel');
                if (root) root.style.display = 'none';
            });
            if (this.onCheckServer) {
                void this.pollServer();
                if (typeof this.setIntervalImpl === 'function') {
                    this.pollTimer = this.setIntervalImpl(() => void this.pollServer(), this.pollIntervalMs);
                }
            }
            return this;
        }

        _query(id) {
            return this.panel?.querySelector?.('#' + id) || null;
        }

        async pollServer() {
            if (!this.onCheckServer) return null;
            const connection = await this.onCheckServer();
            this.setServerStatus(connection);
            return connection;
        }

        setServerStatus(connection = {}) {
            const ok = connection.state === 'connected';
            const service = this._query('acm-srv');
            if (service) {
                service.textContent = ok ? '🟢 运行中' : connection.state === 'secret' ? '🟡 请填写密钥' : '🔴 连接失败';
                service.title = ok ? connection.url || '' : connection.message || '';
                service.style.color = ok ? '#4caf50' : connection.state === 'secret' ? '#ffb300' : '#f44336';
            }
            const syncButton = this._query('acm-sync-btn');
            const fullButton = this._query('acm-full-btn');
            if (syncButton && syncButton.dataset?.syncing !== '1') {
                syncButton.disabled = !ok;
                syncButton.style.opacity = ok ? '1' : '0.5';
            }
            if (fullButton && !fullButton.dataset?.fullDisabled) {
                fullButton.disabled = !ok;
                fullButton.style.opacity = ok ? '1' : '0.5';
            }
        }

        setConnection(connection = {}) {
            this.setServerStatus(connection);
        }

        setProgress(current, total, text) {
            const bar = this._query('acm-bar');
            const status = this._query('acm-status-line');
            const pct = total ? Math.round((current / total) * 100) : 0;
            if (bar) bar.style.width = pct + '%';
            if (status) status.textContent = text || `${current}/${total}`;
        }

        setStatus(text) {
            const status = this._query('acm-status-line');
            if (!status) return;
            status.textContent = text;
            status.style.color = String(text).includes('❌') ? '#f44336'
                : String(text).includes('✅') ? '#4caf50' : '#888';
        }

        setSyncing(active) {
            const syncButton = this._query('acm-sync-btn');
            const fullButton = this._query('acm-full-btn');
            const bar = this._query('acm-bar');
            if (!syncButton) return;
            if (active) {
                syncButton.textContent = '停止同步';
                syncButton.style.background = '#f44336';
                syncButton.dataset.syncing = '1';
                if (fullButton) {
                    fullButton.disabled = true;
                    fullButton.style.opacity = '0.5';
                }
            } else {
                syncButton.textContent = '开始同步';
                syncButton.style.background = '#4caf50';
                syncButton.dataset.syncing = '0';
                if (fullButton) {
                    fullButton.disabled = false;
                    fullButton.style.opacity = '1';
                }
                if (bar) this.setTimeoutImpl(() => { bar.style.width = '0%'; }, 2000);
            }
        }

        destroy() {
            if (this.pollTimer !== null && typeof this.clearIntervalImpl === 'function') {
                this.clearIntervalImpl(this.pollTimer);
            }
            this.pollTimer = null;
            this.panel?.remove?.();
            this.panel = null;
            this.mounted = false;
        }
    }

    const testApi = Object.freeze({
        RuntimeConfig,
        JsonTools,
        CaptureRedactor,
        SseParser,
        DeepSeekExchangeClassifier,
        CaptureStore,
        NetworkCapture,
        attachDeepSeekCapture,
        CredentialStore,
        BridgeClient,
        BaseAdapter,
        DeepSeekAdapter,
        DoubaoAdapter,
        KimiAdapter,
        SyncCoordinator,
        SyncPanel
    });
    if (TEST_MODE) {
        globalThis.__AI_CHAT_MEMORY_TEST_API__ = testApi;
        return;
    }

    // ===== 核心功能 =====
    const credentialStore = new CredentialStore();
    const bridgeClient = new BridgeClient({ credentials: credentialStore });
    const deepSeekCaptureStore = PLATFORM === 'deepseek' ? new CaptureStore() : null;
    const adapter = PLATFORM === 'deepseek'
        ? new DeepSeekAdapter({ captureStore: deepSeekCaptureStore, credentials: credentialStore, location })
        : PLATFORM === 'kimi'
            ? new KimiAdapter({ credentials: credentialStore, location, fetch: globalThis.fetch, window: unsafeWindow })
            : new DoubaoAdapter({ location, fetch: globalThis.fetch });

    if (PLATFORM === 'deepseek') {
        new NetworkCapture({
            window: unsafeWindow,
            store: deepSeekCaptureStore,
            getPath: () => location.pathname,
            onToken: token => credentialStore.saveToken(adapter.tokenKey, token, 'DeepSeek'),
            onPayload: payload => adapter._cacheReferences(payload)
        }).install();
        console.log('🔄 DeepSeek 网页协议捕获器已启动');
    }

    GM_registerMenuCommand('设置后端地址', () => {
        const value = prompt('请输入后端地址；可省略 http:// 和 /api/v1', bridgeClient.configuredUrl);
        if (value === null) return;
        try {
            bridgeClient.setUrl(value);
            alert('后端地址已保存，刷新页面后生效。');
        } catch (error) {
            alert(`后端地址无效: ${error.message}`);
        }
    });
    GM_registerMenuCommand('重置后端地址', () => {
        bridgeClient.resetUrl();
        alert(`后端地址已重置为 ${DEFAULT_BRIDGE_URL}，刷新页面后生效。`);
    });
    GM_registerMenuCommand('设置本地服务密钥', () => {
        const value = prompt('请输入桌面端生成的随机密钥；留空表示清除', credentialStore.getSecret(BRIDGE_SECRET_KEY));
        if (value === null) return;
        const secret = credentialStore.setSecret(BRIDGE_SECRET_KEY, value);
        alert(secret ? '本地服务密钥已保存。' : '本地服务密钥已清除。');
    });
    if (adapter.tokenKey) {
        GM_registerMenuCommand('清除已保存令牌', () => {
            credentialStore.clearToken(adapter.tokenKey);
            alert(`${PLATFORM} 令牌已清除，将自动捕获新的令牌。`);
        });
    }

    let coordinator;
    const ui = new SyncPanel({
        document,
        onSync: () => coordinator.sync(false),
        onFullSync: () => coordinator.sync(true),
        onStop: () => coordinator.stop(),
        onCheckServer: () => bridgeClient.checkServer()
    });
    coordinator = new SyncCoordinator({
        adapter,
        bridgeClient,
        platform: PLATFORM,
        ui,
        fetch: globalThis.fetch
    });

    console.log('✅ AI Chat Memory UI 已注入');
})();
