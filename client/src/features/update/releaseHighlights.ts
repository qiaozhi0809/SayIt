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
  version: '0.0.9',
  items: [
    '新增「文本处理」设置，不依赖 AI 也能规范识别结果里的数字和标点',
    '数字规范化：把中文数字转成阿拉伯数字，如 百分之十五 → 15%、三点一四 → 3.14',
    '可一键去除句末标点，或把标点统一替换为空格',
    '智能分段改为可关闭的开关（默认开启），极速模式下长段语音自动分段更易读',
    '云 API 识别配置简化：千问 ASR 只需填百炼 API Key，App ID 仅豆包显示',
    '提升千问 ASR 流式识别稳定性，减少异常回退到录完再识别',
    '多显示器下悬浮窗显示在录音所在屏幕的底部',
    '写入失败的结果悬浮窗自动关闭更快，点击复制后约 0.5 秒关闭',
  ],
}
