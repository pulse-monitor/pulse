import { get } from '../../api'
import * as f from '../../lib/format'
import { Err, useList } from './ui'

interface AlertEvent {
  id: number
  server_id: number | null
  server_name: string | null
  kind: string
  state: string
  started_at: number
  resolved_at: number | null
  detail: string | null
}

/** 告警事件历史。只读 —— 后台不提供「手动消警」，状态由评估循环自己收敛。 */
export default function Events() {
  const { data, err } = useList(() => get<AlertEvent[]>('/api/v1/admin/events?limit=100'))
  return (
    <div className="space-y-3">
      <Err msg={err} />
      <div className="overflow-x-auto">
        <table className="w-full text-sm">
          <thead className="text-left text-xs text-zinc-500">
            <tr>
              <th className="py-1 pr-3">时间</th>
              <th className="py-1 pr-3">机器</th>
              <th className="py-1 pr-3">事件</th>
              <th className="py-1 pr-3">状态</th>
              <th className="py-1">详情</th>
            </tr>
          </thead>
          <tbody>
            {(data ?? []).map((e) => (
              <tr key={e.id} className="border-t border-black/5 dark:border-white/10">
                <td className="whitespace-nowrap py-1 pr-3 tabular-nums text-zinc-500">
                  {f.time(e.started_at)}
                </td>
                <td className="py-1 pr-3">{e.server_name ?? '—'}</td>
                <td className="py-1 pr-3">{e.kind}</td>
                <td className="py-1 pr-3">
                  {e.resolved_at ? (
                    <span className="text-emerald-600">已恢复</span>
                  ) : (
                    <span className="text-red-500">{e.state}</span>
                  )}
                </td>
                <td className="py-1 text-xs text-zinc-500">{e.detail}</td>
              </tr>
            ))}
          </tbody>
        </table>
        {data?.length === 0 && <p className="py-2 text-sm text-zinc-500">还没有告警事件</p>}
      </div>
    </div>
  )
}
