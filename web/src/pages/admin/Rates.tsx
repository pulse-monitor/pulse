import { useState } from 'react'
import { del, get, post, put } from '../../api'
import { Btn, Card, Err, Field, Num, Text, run, useList } from './ui'

interface RateRow {
  quote: string
  rate: number
  /** manual = 人工填的，会覆盖自动汇率 */
  source: string
}

interface RatesResp {
  as_of: string | null
  fetched_at: number | null
  /** 自动来源覆盖不到、必须人工填的货币 */
  known_unsupported: string[]
  rates: RateRow[]
  /** 自动汇率是否已过期（拉取失败或太久没更新） */
  stale: boolean
}

/**
 * 汇率（R15）。自动汇率覆盖 30 种货币；不在其中的必须人工填，
 * 否则那台机器不计入总价值 —— 前端会在统计条上明说有几台缺汇率。
 */
export default function Rates() {
  const { data, err, reload, setErr } = useList(() => get<RatesResp>('/api/v1/admin/exchange-rates'))
  const [quote, setQuote] = useState('')
  const [rate, setRate] = useState<number | null>(null)
  const [msg, setMsg] = useState<string | null>(null)

  return (
    <div className="space-y-3">
      <Err msg={err} />
      {msg && <p className="text-sm text-emerald-600">{msg}</p>}
      {data && (
        <p className="text-xs text-zinc-500">
          自动汇率日期 {data.as_of ?? '—'}
          {data.stale && <span className="ml-2 text-amber-600">已过期，正在用最后一次成功的值</span>}
          {data.known_unsupported.length > 0 && (
            <span className="ml-2">
              自动来源不覆盖：{data.known_unsupported.join(' ')}（用到这些货币必须在下方人工填）
            </span>
          )}
        </p>
      )}
      <Card title="人工汇率">
        <div className="grid gap-2 sm:grid-cols-3">
          <Field label="货币代码" hint="三位字母，如 RUB">
            <Text value={quote} onChange={(v) => setQuote(v.toUpperCase())} maxLength={3} />
          </Field>
          <Field label="1 USD =" hint="该货币的数量">
            <Num value={rate} onChange={setRate} step="0.0001" min="0" />
          </Field>
        </div>
        <div className="mt-2 flex gap-2">
          <Btn
            kind="primary"
            onClick={() => {
              if (quote.length !== 3) return setErr('货币代码必须是三位字母')
              if (!rate || rate <= 0) return setErr('汇率必须大于 0')
              run(
                () => put(`/api/v1/admin/exchange-rates/${quote}`, { rate }),
                () => {
                  setQuote('')
                  setRate(null)
                  reload()
                },
                (m) => setErr(m || null),
              )
            }}
          >
            设定
          </Btn>
          <Btn
            onClick={() =>
              run(
                async () => {
                  await post('/api/v1/admin/exchange-rates/refresh', {})
                  setMsg('已触发刷新')
                },
                reload,
                (m) => setErr(m || null),
              )
            }
          >
            立即刷新自动汇率
          </Btn>
        </div>
      </Card>

      <div className="overflow-x-auto">
        <table className="w-full text-sm">
          <thead className="text-left text-xs text-zinc-500">
            <tr>
              <th className="py-1 pr-3">货币</th>
              <th className="py-1 pr-3">1 USD =</th>
              <th className="py-1 pr-3">来源</th>
              <th className="py-1" />
            </tr>
          </thead>
          <tbody>
            {(data?.rates ?? []).map((r) => (
              <tr key={r.quote} className="border-t border-black/5 dark:border-white/10">
                <td className="py-1 pr-3 font-medium">{r.quote}</td>
                <td className="py-1 pr-3 tabular-nums">{r.rate.toFixed(4)}</td>
                <td className="py-1 pr-3 text-xs text-zinc-500">
                  {/* 不显示提供方的名字，只区分「人工」和「自动」——
                      换源是实现细节，不该逼用户去理解那个名字是什么 */}
                  {r.source === 'manual' ? <span className="text-amber-600">人工</span> : '自动'}
                </td>
                <td className="py-1">
                  {r.source === 'manual' && (
                    <Btn
                      kind="danger"
                      onClick={() =>
                        run(
                          () => del(`/api/v1/admin/exchange-rates/${r.quote}`),
                          reload,
                          (m) => setErr(m || null),
                        )
                      }
                    >
                      恢复自动
                    </Btn>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  )
}
