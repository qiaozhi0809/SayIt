// 本次版本更新亮点（关于页面展示）。
// 每次发版时更新 version 与 items，保持与 CHANGELOG 同步。
// version 需与打包版本一致，关于页面仅在与当前版本匹配时展示，避免串版。
//
// 每条写成一句话：前半句说明功能，后半句补充解释，读起来更顺。

export interface ReleaseHighlights {
  version: string
  items: string[]
}

export const RELEASE_HIGHLIGHTS: ReleaseHighlights = {
  version: '0.1.0',
  items: [
    '新增「流式实时字幕」：说话时悬浮窗像输入法一样边说边出字，说完仍交给 AI 整理（支持豆包、千问实时）',
    '快捷键支持鼠标侧键：按住说话与免提模式都能绑定鼠标的前进 / 后退键',
    '新增「口语化整理」AI 预设：把语音整理成微信、Teams 那样自然口语、可直接发送的短消息',
    '应用 Prompt 规则内置 Codex、微信、QQ：在对应软件里自动套用更合适的整理风格',
    '诊断页新增「常见问题」：收录「文字插不进输入框」等问题的自助排查步骤',
    '修复千问短音频偶发把热词原样吐出、诊断报告时间显示等小问题',
  ],
}
