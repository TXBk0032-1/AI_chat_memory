// ==UserScript==
// @name         AI Chat Memory - 多平台导出
// @namespace    ai-chat-memory
// @version      1.0.0
// @description  跨平台AI对话导出工具，支持同步到本地服务
// @author       AI Chat Memory
// @match        https://chat.deepseek.com/*
// @match        https://www.doubao.com/*
// @match        https://kimi.com/*
// @match        https://www.kimi.com/*
// @run-at       document-start
// @grant        GM_setValue
// @grant        GM_getValue
// @grant        GM_registerMenuCommand
// @grant        unsafeWindow
// ==/UserScript==

(function() {
    'use strict';

    // ===== 配置 =====
    const BRIDGE_URL = 'http://localhost:19820/api/v1';
    const PLATFORM = location.hostname.includes('deepseek') ? 'deepseek'
                   : location.hostname.includes('doubao') ? 'doubao'
                   : location.hostname.includes('kimi') ? 'kimi' : null;

    if (!PLATFORM) return;
    console.log(`🧠 AI Chat Memory 已加载 [${PLATFORM}]`);

    // ===== 适配器基类 =====
    class BaseAdapter {
        platform = '';
        needsToken = false;
        tokenKey = '';

        async getToken() { return null; }
        async fetchAllSessions() { return []; }
        async fetchConversation(id) { return null; }
        getCurrentSessionId() { return null; }
    }

    // ===== DeepSeek 适配器 =====
    class DeepSeekAdapter extends BaseAdapter {
        platform = 'deepseek';
        needsToken = true;
        tokenKey = 'ds_token';

        constructor() {
            super();
            this._captureToken();
        }

        _captureToken() {
            if (GM_getValue(this.tokenKey) || unsafeWindow.__DS_CAPTURED) return;
            console.log('🔄 令牌窃听器已启动，等待有效 Bearer 令牌...');

            const originalFetch = unsafeWindow.fetch;
            unsafeWindow.fetch = function(...args) {
                const options = args[1] || {};
                const headers = options.headers || {};
                const auth = headers.Authorization || headers.authorization;
                const match = typeof auth === 'string' && auth.match(/^Bearer\s+(.+)$/);
                if (match && match[1].trim() !== '') {
                    console.log('✅ 已捕获令牌(fetch):', match[1].slice(0, 10) + '...');
                    GM_setValue('ds_token', match[1]);
                    unsafeWindow.__DS_CAPTURED = true;
                    unsafeWindow.fetch = originalFetch;
                }
                return originalFetch.apply(this, args);
            };

            const XHR = unsafeWindow.XMLHttpRequest;
            const origOpen = XHR.prototype.open;
            const origSetHeader = XHR.prototype.setRequestHeader;
            const origSend = XHR.prototype.send;

            XHR.prototype.open = function(method, url) { this._url = url; return origOpen.apply(this, arguments); };
            XHR.prototype.setRequestHeader = function(h, v) { if (h.toLowerCase() === 'authorization') this._authHeader = v; return origSetHeader.apply(this, arguments); };
            XHR.prototype.send = function(...args) {
                if (this._authHeader) {
                    const match = this._authHeader.match(/^Bearer\s+(.+)$/);
                    if (match && match[1].trim() !== '') {
                        console.log('✅ 已捕获令牌(XHR):', match[1].slice(0, 10) + '...');
                        GM_setValue('ds_token', match[1]);
                        unsafeWindow.__DS_CAPTURED = true;
                    }
                }
                return origSend.apply(this, args);
            };
        }

        async getToken() {
            return GM_getValue(this.tokenKey);
        }

        _xhr(url, retry = 0) {
            const token = GM_getValue(this.tokenKey);
            return new Promise((resolve, reject) => {
                const xhr = new unsafeWindow.XMLHttpRequest();
                xhr.open('GET', url);
                xhr.withCredentials = true;
                xhr.setRequestHeader('Authorization', `Bearer ${token}`);
                xhr.setRequestHeader('x-client-version', '1.7.0');
                xhr.setRequestHeader('x-app-version', '20241129.1');
                xhr.setRequestHeader('x-client-locale', 'zh_CN');
                xhr.setRequestHeader('x-client-platform', 'web');
                xhr.setRequestHeader('x-client-timezone-offset', '28800');
                xhr.onload = () => {
                    if (xhr.status === 429 && retry < 3) {
                        const wait = (retry + 1) * 15000;
                        console.warn(`⚠️ 429限流，${wait/1000}s 后重试 (${retry+1}/3)`);
                        setTimeout(() => this._xhr(url, retry + 1).then(resolve, reject), wait);
                    } else {
                        resolve(JSON.parse(xhr.responseText));
                    }
                };
                xhr.onerror = () => reject(new Error(`XHR error: ${xhr.status}`));
                xhr.send();
            });
        }

        async fetchAllSessions() {
            const token = await this.getToken();
            if (!token) throw new Error('Token 未就绪');
            const allSessions = [];

            let json = await this._xhr('https://chat.deepseek.com/api/v0/chat_session/fetch_page?lte_cursor.pinned=false');
            let sessions = json?.data?.biz_data?.chat_sessions || [];
            let hasMore = json?.data?.biz_data?.has_more || false;
            allSessions.push(...sessions);
            console.log(`📋 首页 ${sessions.length} 条, hasMore=${hasMore}`);

            if (hasMore) {
                do {
                    const cursor = sessions[sessions.length - 1].updated_at;
                    json = await this._xhr(`https://chat.deepseek.com/api/v0/chat_session/fetch_page?lte_cursor.pinned=false&lte_cursor.updated_at=${cursor}`);
                    sessions = json?.data?.biz_data?.chat_sessions || [];
                    hasMore = json?.data?.biz_data?.has_more || false;
                    allSessions.push(...sessions);
                    console.log(`📋 本页 ${sessions.length} 条, 累计 ${allSessions.length}, hasMore=${hasMore}`);
                } while (hasMore);
            }

            console.log(`📋 会话获取完成，共 ${allSessions.length} 个`);
            return allSessions;
        }

        async fetchConversation(id) {
            return this._xhr(`https://chat.deepseek.com/api/v0/chat/history_messages?chat_session_id=${id}`);
        }

        getCurrentSessionId() {
            return location.pathname.match(/\/s\/([^/?]+)/)?.[1];
        }
    }

    // ===== 豆包适配器 =====
    class DoubaoAdapter extends BaseAdapter {
        platform = 'doubao';
        needsToken = false;
        apiParams = 'version_code=20800&language=zh&device_platform=web&aid=497858&real_aid=497858&pkg_type=release_version&device_id=0&pc_version=3.5.9&samantha_web=1&use-olympus-account=1';

        async fetchAllSessions() {
            const res = await fetch(`https://www.doubao.com/im/chain/recent_conv?${this.apiParams}`, {
                method: 'POST',
                headers: { 'content-type': 'application/json; encoding=utf-8' },
                body: JSON.stringify({
                    cmd: 3200,
                    uplink_body: { pull_recent_conv_chain_uplink_body: { limit: 100, api_version: 1, direction: 3, option: { not_need_message: true, need_complete_conversation: true } } },
                    sequence_id: crypto.randomUUID(),
                    channel: 2,
                    version: "1"
                })
            });
            const json = await res.json();
            return json?.downlink_body?.pull_recent_conv_chain_downlink_body?.cells || [];
        }

        async fetchConversation(id) {
            const res = await fetch(`https://www.doubao.com/im/chain/single?${this.apiParams}`, {
                method: 'POST',
                headers: { 'content-type': 'application/json; encoding=utf-8' },
                body: JSON.stringify({
                    cmd: 3100,
                    uplink_body: { pull_singe_chain_uplink_body: { conversation_id: id, conversation_type: 3, anchor_index: 9007199254740991, direction: 1, limit: 1000 } },
                    sequence_id: crypto.randomUUID(),
                    channel: 2,
                    version: "1"
                })
            });
            return res.json();
        }

        getCurrentSessionId() {
            return location.pathname.match(/\/chat\/(\d+)/)?.[1];
        }
    }

    // ===== Kimi 适配器 =====
    class KimiAdapter extends BaseAdapter {
        platform = 'kimi';
        needsToken = true;
        tokenKey = 'kimi_token';

        constructor() {
            super();
            this._captureToken();
        }

        _captureToken() {
            if (unsafeWindow.__KIMI_CAPTURED) return;
            const originalFetch = unsafeWindow.fetch;
            const tokenKey = this.tokenKey;
            unsafeWindow.fetch = function(...args) {
                try {
                    const options = args[1] || {};
                    let auth = null;
                    if (options.headers) {
                        auth = typeof options.headers.get === 'function'
                            ? options.headers.get('authorization')
                            : (options.headers.authorization || options.headers.Authorization);
                    }
                    const match = typeof auth === 'string' && auth.match(/^Bearer\s+(.+)$/);
                    if (match && match[1].trim() !== '') {
                        console.log('✅ 已捕获Kimi令牌:', match[1].slice(0, 10) + '...');
                        GM_setValue(tokenKey, match[1]);
                        unsafeWindow.__KIMI_CAPTURED = true;
                    }
                } catch(e) {}
                return originalFetch.apply(this, args);
            };
        }

        async getToken() { return GM_getValue(this.tokenKey); }

        async _fetch(url, body) {
            const token = GM_getValue(this.tokenKey);
            const res = await fetch(url, {
                method: 'POST',
                headers: {
                    'Content-Type': 'application/json',
                    'Authorization': `Bearer ${token}`,
                    'x-msh-platform': 'web'
                },
                credentials: 'include',
                body: JSON.stringify(body)
            });
            if (res.status === 429) throw new Error('Kimi 429 限流');
            return res.json();
        }

        async fetchAllSessions() {
            const token = await this.getToken();
            if (!token) throw new Error('Kimi Token 未就绪');
            const all = [];
            let pageToken = '';
            do {
                const body = { project_id: '', page_size: 200, query: '' };
                if (pageToken) body.page_token = pageToken;
                const json = await this._fetch('https://www.kimi.com/apiv2/kimi.chat.v1.ChatService/ListChats', body);
                const chats = json.chats || [];
                all.push(...chats);
                pageToken = json.nextPageToken || '';
                console.log(`📋 Kimi: 本页 ${chats.length} 条, 累计 ${all.length}`);
            } while (pageToken);
            return all;
        }

        async fetchConversation(id) {
            const json = await this._fetch('https://www.kimi.com/apiv2/kimi.gateway.chat.v1.ChatService/ListMessages', { chat_id: id, page_size: 1000 });
            return json;
        }

        getCurrentSessionId() {
            return location.pathname.match(/\/chat\/([^/?]+)/)?.[1];
        }
    }

    // ===== 核心功能 =====
    const adapter = PLATFORM === 'deepseek' ? new DeepSeekAdapter()
                  : PLATFORM === 'kimi' ? new KimiAdapter()
                  : new DoubaoAdapter();

    async function checkServer() {
        try {
            const res = await fetch(`${BRIDGE_URL}/health`);
            return res.ok;
        } catch { return false; }
    }

    async function fetchSessionsIncremental(lastUpdatedAt) {
        const token = await adapter.getToken();
        if (adapter.needsToken && !token) throw new Error('Token 未就绪');
        const newSessions = [];
        let cursor = null, hasMore = true;

        while (hasMore) {
            let url = 'https://chat.deepseek.com/api/v0/chat_session/fetch_page?lte_cursor.pinned=flase';
            if (cursor) url += `&lte_cursor.updated_at=${cursor}`;
            const json = await adapter._xhr(url);
            const sessions = json?.data?.biz_data?.chat_sessions || [];
            hasMore = json?.data?.biz_data?.has_more || false;

            let hitOld = false;
            for (const s of sessions) {
                if (s.pinned) {
                    if (s.updated_at > lastUpdatedAt) newSessions.push(s);
                    continue;
                }
                if (lastUpdatedAt && s.updated_at <= lastUpdatedAt) {
                    hitOld = true;
                    break;
                }
                newSessions.push(s);
            }
            if (hitOld || !sessions.length) break;
            cursor = sessions[sessions.length - 1].updated_at;
            console.log(`📋 增量: 本页 ${sessions.length} 条, 新会话累计 ${newSessions.length}`);
        }
        return newSessions;
    }

    let syncAbort = false;

    async function fetchDetailsAndPush(sessions) {
        if (!sessions.length) {
            ui.setStatus('✅ 无新会话需要同步');
            return;
        }
        const CONCURRENCY = 4, DELAY = 50;
        const queue = [...sessions];
        const results = [];
        let counter = 0;

        async function worker() {
            while (queue.length > 0 && !syncAbort) {
                const s = queue.shift();
                const id = PLATFORM === 'doubao' ? s.conversation?.conversation_id : s.id;
                const n = ++counter;
                ui.setProgress(n, sessions.length, `获取详情 ${n}/${sessions.length}`);
                const conv = await adapter.fetchConversation(id);
                results.push({ ...s, _conversation: conv });
                await new Promise(r => setTimeout(r, DELAY));
            }
        }
        await Promise.all(Array(CONCURRENCY).fill().map(() => worker()));
        if (syncAbort) { ui.setStatus('⏹ 已停止'); return; }

        ui.setProgress(1, 1, '推送到服务端...');
        const res = await fetch(`${BRIDGE_URL}/sessions/import`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ platform: PLATFORM, sessions: results })
        });
        const data = await res.json();
        ui.setStatus(`✅ 导入 ${data.imported} 个, 跳过 ${data.skipped} 个`);
    }

    async function syncToServer(fullSync = false) {
        if (!await checkServer()) {
            ui.setStatus('❌ 服务未运行');
            return;
        }
        syncAbort = false;
        ui.setSyncing(true);
        try {
            let sessions;
            if (fullSync) {
                ui.setStatus('全量拉取会话列表...');
                sessions = await adapter.fetchAllSessions();
                ui.setStatus(`全量获取 ${sessions.length} 个会话`);
            } else {
                ui.setStatus('查询同步状态...');
                const statusRes = await fetch(`${BRIDGE_URL}/sessions/sync-status?platform=${PLATFORM}`);
                const { last_updated_at } = await statusRes.json();

                if (!last_updated_at) {
                    ui.setStatus('本地为空，全量拉取...');
                    sessions = await adapter.fetchAllSessions();
                } else if (PLATFORM === 'deepseek') {
                    ui.setStatus('增量拉取...');
                    sessions = await fetchSessionsIncremental(last_updated_at);
                } else {
                    sessions = await adapter.fetchAllSessions();
                }
                ui.setStatus(`需同步 ${sessions.length} 个会话`);
            }
            if (syncAbort) { ui.setStatus('⏹ 已停止'); return; }
            await fetchDetailsAndPush(sessions);
        } catch (e) {
            ui.setStatus('❌ ' + e.message);
        } finally {
            ui.setSyncing(false);
        }
    }

    // ===== UI 面板 =====
    const ui = (() => {
        const panel = document.createElement('div');
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
        document.addEventListener('DOMContentLoaded', () => document.body.appendChild(panel));
        if (document.body) document.body.appendChild(panel);

        const $ = id => panel.querySelector('#' + id);
        const syncBtn = () => $('acm-sync-btn');
        const fullBtn = () => $('acm-full-btn');
        const bar = () => $('acm-bar');
        const statusLine = () => $('acm-status-line');
        const srvSpan = () => $('acm-srv');

        // 服务状态轮询
        async function pollServer() {
            const ok = await checkServer();
            const el = srvSpan();
            if (el) { el.textContent = ok ? '🟢 运行中' : '🔴 未连接'; el.style.color = ok ? '#4caf50' : '#f44336'; }
            const sb = syncBtn(), fb = fullBtn();
            if (sb && sb.dataset.syncing !== '1') { sb.disabled = !ok; sb.style.opacity = ok ? '1' : '0.5'; }
            if (fb && !fb.dataset.fullDisabled) { fb.disabled = !ok; fb.style.opacity = ok ? '1' : '0.5'; }
        }
        pollServer();
        setInterval(pollServer, 18000);

        // 按钮事件
        const bindOnce = () => {
            const sb = syncBtn(), fb = fullBtn();
            if (!sb) return;
            sb.onclick = () => {
                if (sb.dataset.syncing === '1') { syncAbort = true; return; }
                syncToServer(false);
            };
            fb.onclick = () => {
                if (fb.disabled) return;
                syncToServer(true);
            };
            const closeBtn = $('acm-close');
            if (closeBtn) closeBtn.onclick = () => { $('acm-panel').style.display = 'none'; };
        };
        document.addEventListener('DOMContentLoaded', bindOnce);
        if (document.body) bindOnce();

        return {
            setProgress(current, total, text) {
                const pct = Math.round((current / total) * 100);
                const b = bar(); if (b) b.style.width = pct + '%';
                const s = statusLine(); if (s) s.textContent = text || `${current}/${total}`;
            },
            setStatus(text) {
                const s = statusLine(); if (!s) return;
                s.textContent = text;
                s.style.color = text.includes('❌') ? '#f44336' : text.includes('✅') ? '#4caf50' : '#888';
            },
            setSyncing(active) {
                const sb = syncBtn(), fb = fullBtn(), b = bar();
                if (!sb) return;
                if (active) {
                    sb.textContent = '停止同步'; sb.style.background = '#f44336'; sb.dataset.syncing = '1';
                    fb.disabled = true; fb.style.opacity = '0.5';
                } else {
                    sb.textContent = '开始同步'; sb.style.background = '#4caf50'; sb.dataset.syncing = '0';
                    fb.disabled = false; fb.style.opacity = '1';
                    if (b) setTimeout(() => { b.style.width = '0%'; }, 2000);
                }
            }
        };
    })();

    console.log('✅ AI Chat Memory UI 已注入');
})();
