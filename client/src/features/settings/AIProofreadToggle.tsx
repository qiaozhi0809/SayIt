// AI 校对开关卡片（独立组件）
// 状态接入全局 store，与首页、标题栏的开关保持同步

import { useEffect, useState } from 'react'
import { Card, CardContent } from '@/components/ui/card'
import { Switch } from '@/components/ui/switch'
import { getSetting } from '@/services/store'
import { getWorkMode } from '@/services/transcription'
import { useAiEnabled } from '@/hooks/useAiEnabled'
import { toggleAiEnabled } from '@/stores/aiEnabled'

export default function AIProofreadToggle() {
  const aiEnabled = useAiEnabled()
  const [workMode, setWorkMode] = useState(getWorkMode)

  useEffect(() => {
    getSetting('workMode', 'server').then((v) => {
      const m = v as string
      if (m === 'server' || m === 'cloud_api' || m === 'local') setWorkMode(m as typeof workMode)
    })
  }, [])

  return (
    <Card>
      <CardContent className="p-6">
        <div className="flex items-center justify-between">
          <div>
            <h2 className="text-lg font-semibold">AI 整理</h2>
            <p className="mt-1 text-xs text-muted-foreground">
              {aiEnabled
                ? workMode === 'server'
                  ? '开启后，会自动整理口述内容，修正错字、整理语序（由服务器端 AI 处理）'
                  : '开启后，会自动整理口述内容，修正错字、整理语序（由下方配置的 AI 供应商处理）'
                : '关闭后，将原样输出语音识别的文字，不做修改'}
            </p>
          </div>
          <Switch checked={aiEnabled} onChange={() => { void toggleAiEnabled() }} />
        </div>
      </CardContent>
    </Card>
  )
}
