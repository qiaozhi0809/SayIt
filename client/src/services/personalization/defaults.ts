import type { AppPromptRule, UserStats } from './types'

export const BUILTIN_APP_RULES: AppPromptRule[] = [
  {
    id: 'teams',
    appId: 'teams',
    name: 'Teams',
    builtin: true,
    enabled: false,
    priority: 100,
    presetId: 'intent',
    promptAppend: '适合即时协作聊天。优先输出可以直接发送的短消息，语气自然、清晰、简洁，避免邮件腔。',
    matcher: {
      processNames: ['teams.exe', 'ms-teams.exe'],
      windowTitleIncludes: ['teams'],
    },
  },
  {
    id: 'outlook',
    appId: 'outlook',
    name: 'Outlook',
    builtin: true,
    enabled: false,
    priority: 95,
    presetId: 'intent',
    promptAppend: '适合工作邮件草稿。语气正式、完整，必要时自然分段，但不要编造收件人、称呼或任何事实。',
    matcher: {
      processNames: ['outlook.exe', 'olk.exe'],
      windowTitleIncludes: ['outlook'],
    },
  },
  {
    id: 'kiro',
    appId: 'kiro',
    name: 'Kiro',
    builtin: true,
    enabled: false,
    priority: 90,
    presetId: 'faithful',
    promptAppend: '面向开发工具输入。保留代码、命令、文件名、路径、英文标识符和 Markdown 结构，不要把技术词改写成普通中文。',
    matcher: {
      processNames: ['kiro.exe'],
      windowTitleIncludes: ['kiro'],
    },
  },
  {
    id: 'vscode',
    appId: 'vscode',
    name: 'VSCode',
    builtin: true,
    enabled: false,
    priority: 85,
    presetId: 'faithful',
    promptAppend: '面向 VSCode 编辑区。保留代码、命令、文件名、API、英文术语和 Markdown 结构，不要过度润色技术内容。',
    matcher: {
      processNames: ['code.exe'],
      windowTitleIncludes: ['visual studio code', 'vscode'],
    },
  },
  {
    id: 'cursor',
    appId: 'cursor',
    name: 'Cursor',
    builtin: true,
    enabled: false,
    priority: 80,
    presetId: 'faithful',
    promptAppend: '面向 Cursor 编辑区。保留代码、命令、路径、技术名词和英文标识符，不要擅自解释或补全技术内容。',
    matcher: {
      processNames: ['cursor.exe'],
      windowTitleIncludes: ['cursor'],
    },
  },
  {
    id: 'notepad',
    appId: 'notepad',
    name: '记事本',
    builtin: true,
    enabled: false,
    priority: 70,
    presetId: 'intent',
    promptAppend: '面向 Windows 记事本，适合随手记录的纯文本。输出纯文本，不要使用 Markdown 标记或特殊格式符号。',
    matcher: {
      processNames: ['notepad.exe'],
      windowTitleIncludes: ['记事本', 'notepad'],
    },
  },
  {
    id: 'codex',
    appId: 'codex',
    name: 'Codex',
    builtin: true,
    enabled: false,
    priority: 88,
    presetId: 'faithful',
    promptAppend: '面向 Codex 编码工具，多为用自然语言下达编程指令。保留代码、命令、文件名、路径和英文标识符，把要做的事说清楚，不要把技术词改写成普通中文。',
    matcher: {
      processNames: ['codex.exe'],
      windowTitleIncludes: ['codex'],
    },
  },
  {
    id: 'weixin',
    appId: 'weixin',
    name: '微信',
    builtin: true,
    enabled: false,
    priority: 68,
    presetId: 'casual',
    promptAppend: '适合微信聊天。输出可直接发送的自然口语短消息，简洁亲切，不要用书面或邮件腔。',
    matcher: {
      processNames: ['weixin.exe', 'wechat.exe'],
      windowTitleIncludes: ['微信', 'weixin', 'wechat'],
    },
  },
  {
    id: 'qq',
    appId: 'qq',
    name: 'QQ',
    builtin: true,
    enabled: false,
    priority: 66,
    presetId: 'casual',
    promptAppend: '适合 QQ 聊天。输出轻松自然、可直接发送的短消息，口语化、简洁。',
    matcher: {
      processNames: ['qq.exe'],
      windowTitleIncludes: ['qq'],
    },
  },
]

export function createDefaultUserStats(): UserStats {
  return {
    totalWords: 0,
    totalSessions: 0,
    domainWords: {},
    appUsageCount: {},
  }
}
