// 文本格式规范开关 — 不依赖 AI 的客户端文本处理

import { useEffect, useRef, useState } from 'react'
import { HelpCircle } from 'lucide-react'
import { Switch } from '@/components/ui/switch'
import { Tooltip } from '@/components/ui/tooltip'
import {
  getTextPostProcessOptions,
  saveTextPostProcessOptions,
  DEFAULT_POST_PROCESS,
  type TextPostProcessOptions,
} from '@/services/textPostProcess'

interface ToggleDef {
  key: keyof TextPostProcessOptions
  title: string
  /** 行内可见的简短说明 */
  hint: string
  /** hover 问号显示的详细说明 + 例子（换行用 \n） */
  detail: string
}

const TOGGLES: ToggleDef[] = [
  {
    key: 'autoSegment',
    title: '智能分段',
    hint: '关闭 AI 整理时，把一大段语音按语义自动分成多个自然段',
    detail:
      '在不使用 AI 整理的极速模式下，根据话题转换、句末停顿，把一整段没有换行的识别结果拆成多个自然段，提升可读性。\n\n开启 AI 整理时，分段交由 AI 完成，此开关不生效。\n\n默认开启。',
  },
  {
    key: 'normalizeNumbers',
    title: '数字规范化',
    hint: '把中文数字改写成阿拉伯数字，如 三点一四 → 3.14、百分之十五 → 15%',
    detail:
      '识别结果里读出来的中文数字，改写成阿拉伯数字，更适合技术、数据类内容。仅在信号明确时转换，尽力而为，复杂或有歧义的场景建议开启 AI 整理。\n\n三点一四 → 3.14\n百分之十五 → 15%\nGPT五点四 → GPT 5.4\n扩容二十三台 → 扩容23台\n两百五 → 250\n\n为避免误伤，孤立的「一二三」、成语（十全十美）、逐位读的号码不会转换。',
  },
  {
    key: 'stripTrailingPunctuation',
    title: '去除句末标点',
    hint: '只删掉每段结尾的标点，句子中间的标点保留，如「你好，世界。」→「你好，世界」',
    detail:
      '只去掉每一段结尾处的标点符号，句子中间的标点（逗号、顿号等）保持不变。适合把识别结果直接发到聊天框、不想带句末句号的场景。\n\n有多段（换行）时，每一段的结尾都会处理，不只是最后一段。\n\n你好，世界。→ 你好，世界\n真的吗？！→ 真的吗\n第一段。↵第二段！→ 第一段↵第二段',
  },
  {
    key: 'punctuationToSpace',
    title: '标点替换为空格',
    hint: '把所有标点都换成空格，让内容以空格分隔，如「你好，世界！」→「你好 世界」',
    detail:
      '把文本里的所有标点符号都替换成空格，并合并多余空格、去掉首尾空格。适合当作关键词、搜索词，或粘贴到不希望带标点的地方。数字里的小数点和百分号会保留。\n\n你好，世界！→ 你好 世界\n第一，先做A；第二，再做B。→ 第一 先做A 第二 再做B\n准确率是99.5%。→ 准确率是99.5%',
  },
]

export default function TextFormatSection() {
  const [opts, setOpts] = useState<TextPostProcessOptions>(DEFAULT_POST_PROCESS)
  const initialized = useRef(false)

  useEffect(() => {
    getTextPostProcessOptions().then((loaded) => {
      setOpts(loaded)
      initialized.current = true
    })
  }, [])

  useEffect(() => {
    if (!initialized.current) return
    void saveTextPostProcessOptions(opts)
  }, [opts])

  const toggle = (key: keyof TextPostProcessOptions) => {
    setOpts((prev) => ({ ...prev, [key]: !prev[key] }))
  }

  return (
    <div className="mb-6 rounded-lg border border-border">
      <div className="border-b border-border px-4 py-3">
        <h2 className="text-lg font-semibold">格式规范</h2>
        <p className="mt-0.5 text-xs text-muted-foreground">不依赖 AI，即使关闭 AI 整理也生效</p>
      </div>
      <div className="divide-y divide-border/60">
        {TOGGLES.map((t) => (
          <div key={t.key} className="flex items-center gap-2.5 px-4 py-2.5">
            <Switch
              checked={opts[t.key]}
              onChange={() => toggle(t.key)}
              size="sm"
              className="shrink-0"
            />
            <span className="shrink-0 text-sm font-medium">{t.title}</span>
            <div className="flex min-w-0 flex-1 items-center gap-1.5">
              <span className="min-w-0 truncate text-xs text-muted-foreground">{t.hint}</span>
              <Tooltip content={t.detail} variant="light">
                <HelpCircle className="h-3.5 w-3.5 shrink-0 cursor-help text-muted-foreground/50 hover:text-muted-foreground" />
              </Tooltip>
            </div>
          </div>
        ))}
      </div>
    </div>
  )
}
