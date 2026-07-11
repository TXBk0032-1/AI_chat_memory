<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { open } from '@tauri-apps/plugin-dialog'
import { BookOpen, Download, RefreshCw, Search, Settings, Trash2, X } from 'lucide-vue-next'
import MarkdownIt from 'markdown-it'
import katex from 'katex'
import 'katex/dist/katex.min.css'
import './style.css'

type Summary={id:string;platform:string;platform_session_id:string;title:string;created_at?:string;updated_at?:string;imported_at?:string}
type Message={id:string;role:string;content:string;metadata:Record<string,unknown>;created_at?:string;seq:number}
type Detail=Summary&{messages:Message[];raw_data?:unknown}
type SettingsModel={setup_complete:boolean;secret_enabled:boolean;secret?:string;allowed_origins:string[];migrated_legacy_database:boolean}

const md=new MarkdownIt({html:false,linkify:true,breaks:true})
const sessions=ref<Summary[]>([]), selected=ref<Detail|null>(null), loading=ref(false), error=ref('')
const query=ref(''), platform=ref(''), dateFrom=ref(''), dateTo=ref(''), showSettings=ref(false), showBranches=ref(false)
const settings=ref<SettingsModel>({setup_complete:false,secret_enabled:false,allowed_origins:[],migrated_legacy_database:false})
const originText=ref('')
const platforms=computed(()=>new Set(sessions.value.map(s=>s.platform)).size)

async function load(){loading.value=true;error.value='';try{const result=await invoke<{sessions:Summary[]}>('search_sessions',{query:{q:query.value||null,platform:platform.value||null,date_from:dateFrom.value||null,date_to:dateTo.value||null,limit:500,offset:0}});sessions.value=result.sessions}catch(e){error.value=String(e)}finally{loading.value=false}}
async function view(id:string){try{selected.value=await invoke<Detail>('get_session',{id});showBranches.value=false}catch(e){error.value=String(e)}}
async function remove(id:string){if(!confirm('确认删除此卷？'))return;await invoke('delete_session',{id});selected.value=null;await load()}
async function importZip(){const path=await open({multiple:false,filters:[{name:'DeepSeek ZIP',extensions:['zip']}]});if(typeof path==='string'){loading.value=true;try{await invoke('import_deepseek_zip',{path});await load()}catch(e){error.value=String(e)}finally{loading.value=false}}}
async function openSettings(){settings.value=await invoke('get_settings');originText.value=settings.value.allowed_origins.join('\n');showSettings.value=true}
async function saveSettings(){settings.value.allowed_origins=originText.value.split('\n').map(v=>v.trim()).filter(Boolean);settings.value.setup_complete=true;settings.value=await invoke('save_settings',{settings:settings.value});showSettings.value=false}
async function rotateSecret(){settings.value=await invoke('rotate_secret');settings.value.setup_complete=true}
function formatTime(value?:string){if(!value)return '';const num=Number(value);const date=Number.isFinite(num)?new Date(num*1000):new Date(value);return Number.isNaN(date.valueOf())?value:date.toLocaleString('zh-CN')}
function render(text:string){let value=md.render(text||'');value=value.replace(/\$\$([\s\S]+?)\$\$/g,(_,t)=>katex.renderToString(t,{displayMode:true,throwOnError:false}));return value}
function meta(m:Message,key:string){return m.metadata?.[key] as string|undefined}
function branchDepth(m:Message){let depth=0,parent=meta(m,'parent_node_id');const map=new Map(selected.value?.messages.map(x=>[meta(x,'node_id'),x]));while(parent&&map.has(parent)&&depth<12){depth++;parent=meta(map.get(parent)!,'parent_node_id')}return depth}
onMounted(async()=>{settings.value=await invoke('get_settings');if(!settings.value.setup_complete){originText.value=settings.value.allowed_origins.join('\n');showSettings.value=true}await load()})
</script>

<template>
  <main class="shell">
    <header>
      <div class="brand"><BookOpen :size="26"/><div><h1>藏经阁</h1><span>AI CHAT MEMORY</span></div></div>
      <div class="stats"><b>{{sessions.length}}</b> 卷 · <b>{{platforms}}</b> 平台</div>
      <div class="actions"><button title="导入 DeepSeek ZIP" @click="importZip"><Download/></button><button title="刷新" @click="load"><RefreshCw/></button><button title="设置" @click="openSettings"><Settings/></button></div>
    </header>
    <section class="toolbar"><label class="search"><Search/><input v-model="query" placeholder="搜索标题或对话内容" @keyup.enter="load"></label><select v-model="platform" @change="load"><option value="">全部平台</option><option value="deepseek">DeepSeek</option><option value="doubao">豆包</option><option value="kimi">Kimi</option></select><input v-model="dateFrom" type="date" @change="load"><input v-model="dateTo" type="date" @change="load"></section>
    <p v-if="error" class="error">{{error}}</p>
    <section class="list" :class="{loading}"><button v-for="s in sessions" :key="s.id" class="session" @click="view(s.id)"><i :class="s.platform"></i><div><strong>{{s.title||'无标题'}}</strong><span>{{formatTime(s.updated_at)}}</span></div><em>{{s.platform}}</em></button><div v-if="!loading&&!sessions.length" class="empty">藏经阁空空如也</div></section>

    <div v-if="selected" class="overlay" @click.self="selected=null"><article class="modal"><div class="modal-head"><div><h2>{{selected.title||'无标题'}}</h2><span>{{selected.platform}} · {{selected.messages.length}} 条消息 · {{formatTime(selected.updated_at)}}</span></div><button @click="selected=null"><X/></button></div><div v-if="selected.messages.some(m=>meta(m,'source')==='deepseek_export')" class="tabs"><button :class="{active:!showBranches}" @click="showBranches=false">时间线</button><button :class="{active:showBranches}" @click="showBranches=true">分支</button></div><div class="messages"><template v-if="!showBranches"><section v-for="m in selected.messages" :key="m.id" class="message" :class="m.role"><div class="message-meta"><b>{{m.role}}</b><span>{{formatTime(m.created_at)}}</span></div><details v-if="meta(m,'thinking')"><summary>思考过程</summary><div v-html="render(meta(m,'thinking')||'')"></div></details><div class="content" v-html="render(m.content)"></div></section></template><template v-else><button v-for="m in selected.messages" :key="m.id" class="branch" :style="{marginLeft:`${branchDepth(m)*22}px`}"><b>{{m.role}}</b><span>{{m.content||meta(m,'thinking')||'空内容'}}</span></button></template></div><button class="danger" @click="remove(selected.id)"><Trash2/> 删除会话</button></article></div>

    <div v-if="showSettings" class="overlay"><article class="settings-modal"><h2>{{settings.setup_complete?'设置':'首次启动设置'}}</h2><p>Origin 白名单（每行一个，不允许通配符）</p><textarea v-model="originText"></textarea><label class="toggle"><input v-model="settings.secret_enabled" type="checkbox"> 启用 userscript 随机密钥</label><div v-if="settings.secret_enabled" class="secret"><code>{{settings.secret||'保存后生成'}}</code><button @click="rotateSecret">轮换</button></div><p class="hint">固定客户端标识不是秘密；随机密钥启用后需同步配置到 userscript。</p><div class="dialog-actions"><button v-if="settings.setup_complete" @click="showSettings=false">取消</button><button class="primary" @click="saveSettings">保存</button></div></article></div>
  </main>
</template>
