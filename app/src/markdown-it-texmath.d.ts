declare module 'markdown-it-texmath' {
  import type MarkdownIt from 'markdown-it'
  const plugin: MarkdownIt.PluginWithOptions<Record<string, unknown>>
  export default plugin
}
