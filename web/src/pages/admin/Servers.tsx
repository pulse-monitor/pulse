import { useState } from 'react'
import { del, get, post, put } from '../../api'
import * as f from '../../lib/format'
import { Flag } from '../../components/Flag'
import { OsIcon } from '../../components/OsIcon'
import ServerEdit from './ServerEdit'
import { Btn, Card, Check, Err, Num, Text, run, useList } from './ui'

interface AdminServer {
  id: number
  uuid: string
  name: string
  os: string | null
  arch: string | null
  agent_version: string | null
  last_seen_at: number | null
  hidden: boolean
  group_id: number | null
  country_code: string | null
  last_ip: string | null
  note: string | null
}

interface Group {
  id: number
  name: string
}

interface InstallCommands {
  shell: string
  powershell: string
  docker: string
  compose: string
  note: string
}

export default function Servers() {
  const { data, err, reload, setErr } = useList(() => get<AdminServer[]>('/api/v1/admin/servers'))
  const groups = useList(() => get<Group[]>('/api/v1/admin/groups'))
  const [name, setName] = useState('')
  const [q, setQ] = useState('')
  const [installing, setInstalling] = useState<number | null>(null)
  const [open, setOpen] = useState<number | null>(null)

  const k = q.trim().toLowerCase()
  const shown = (data ?? []).filter(
    (s) =>
      !k ||
      s.name.toLowerCase().includes(k) ||
      (s.last_ip ?? '').includes(k) ||
      (s.os ?? '').toLowerCase().includes(k),
  )

  const add = () => {
    if (!name.trim()) return
    run(
      () => post('/api/v1/admin/servers', { name: name.trim() }),
      () => {
        setName('')
        reload()
      },
      (m) => setErr(m || null),
    )
  }

  return (
    <div className="space-y-3">
      <Err msg={err} />
      <Card>
        <div className="flex gap-2">
          <Text value={name} onChange={setName} placeholder="新机器的名称" />
          <Btn kind="primary" onClick={add} className="shrink-0">
            添加
          </Btn>
        </div>
      </Card>

      {/* 搜索：机器一多就必须有。按名称、IP、系统都能搜 */}
      <div className="flex flex-wrap items-center gap-2">
        <input
          value={q}
          onChange={(e) => setQ(e.target.value)}
          placeholder="搜索名称 / IP / 系统"
          aria-label="搜索服务器"
          className="w-56 rounded-lg border border-black/15 bg-transparent px-2 py-1.5 text-sm dark:border-white/15"
        />
        <span className="text-xs text-zinc-500">
          {shown.length} / {(data ?? []).length} 台
        </span>
      </div>

      <ul className="space-y-2">
        {shown.map((s) => (
          <li
            key={s.id}
            className="panel p-3 text-sm"
          >
            <div className="flex flex-wrap items-center gap-2">
              <Flag code={s.country_code} />
              <OsIcon os={s.os} />
              <span className="font-medium">{s.name}</span>
              {s.hidden && <span className="text-xs text-amber-600">公开页已隐藏</span>}

              {/* IP 带复制按钮 —— 装机、排查时最常要复制的就是它 */}
              {s.last_ip && (
                <button
                  onClick={() => navigator.clipboard?.writeText(s.last_ip!)}
                  title="复制 IP"
                  className="rounded bg-black/5 px-1.5 py-0.5 font-mono text-xs text-zinc-600 hover:bg-black/10 dark:bg-white/10 dark:text-zinc-400 dark:hover:bg-white/20"
                >
                  {s.last_ip} ⧉
                </button>
              )}

              {/* 分组直接在列表里改，不用单独进编辑页 */}
              <select
                value={s.group_id == null ? '' : String(s.group_id)}
                onChange={(e) =>
                  run(
                    () =>
                      put(`/api/v1/admin/servers/${s.id}`, {
                        group_id: e.target.value === '' ? null : Number(e.target.value),
                      }),
                    reload,
                    (m) => setErr(m || null),
                  )
                }
                aria-label={`${s.name} 的分组`}
                className="rounded border border-black/15 bg-transparent px-1 py-0.5 text-xs dark:border-white/15"
              >
                <option value="">未分组</option>
                {(groups.data ?? []).map((g) => (
                  <option key={g.id} value={g.id}>
                    {g.name}
                  </option>
                ))}
              </select>

              <span className="text-xs text-zinc-500">
                {s.agent_version ? `v${s.agent_version}` : '未安装探针'}
                {s.last_seen_at ? ` · ${f.time(s.last_seen_at)}` : ''}
              </span>

              <span className="ml-auto flex gap-1">
                <Btn onClick={() => setOpen(open === s.id ? null : s.id)}>
                  {open === s.id ? '收起' : '编辑'}
                </Btn>
                <Btn onClick={() => setInstalling(installing === s.id ? null : s.id)}>
                  {installing === s.id ? '收起命令' : '安装命令'}
                </Btn>
                <Btn
                  kind="danger"
                  onClick={() =>
                    confirm(`删除「${s.name}」？它的全部历史数据也会一并删除，且不可恢复。`) &&
                    run(() => del(`/api/v1/admin/servers/${s.id}`), reload, (m) => setErr(m || null))
                  }
                >
                  删除
                </Btn>
              </span>
            </div>

            {installing === s.id && (
              <Install id={s.id} installed={!!s.agent_version} setErr={setErr} />
            )}
            {open === s.id && (
              <ServerEdit id={s.id} name={s.name} groups={groups.data ?? []} onSaved={reload} />
            )}
          </li>
        ))}
        {shown.length === 0 && (
          <li className="text-sm text-zinc-500">
            {(data ?? []).length === 0 ? '还没有机器，先在上面添加一台' : '没有匹配的机器'}
          </li>
        )}
      </ul>
    </div>
  )
}

const DEFAULTS = {
  install_dir: '/opt/pulse-agent',
  disable_auto_update: false,
  interval_s: 3,
  net_include: '',
  net_exclude: '',
  enable_gpu: false,
  include_buffcache: false,
  traffic_reset_day: 1,
}

/**
 * 安装命令与它的选项（R3）。
 *
 * 选项先选、命令后生成 —— 生成出来的一行命令里已经带好了参数，
 * 用户不用装完再去改配置文件。
 */
/**
 * 生成安装命令。
 *
 * **点这个按钮会换掉这台机器的 token**（库里只存哈希，取不回明文，
 * 所以只能重新签发一把）。已经装了探针的机器会当场掉线，直到用新命令重装 ——
 * 所以对这类机器要在点之前就问一句，而不是等命令生成出来再在下面写一行提示。
 */
function Install({
  id,
  installed,
  setErr,
}: {
  id: number
  installed: boolean
  setErr: (m: string | null) => void
}) {
  const [o, setO] = useState(DEFAULTS)
  const [c, setC] = useState<InstallCommands | null>(null)
  const set = (p: Partial<typeof DEFAULTS>) => setO({ ...o, ...p })
  const toList = (v: string) =>
    v
      .split(/[,\s]+/)
      .map((x) => x.trim())
      .filter(Boolean)

  return (
    <div className="mt-2 space-y-2 border-t border-black/10 pt-2 dark:border-white/10">
      <div className="grid gap-2 sm:grid-cols-3">
        <label className="block">
          <span className="mb-1 block text-xs text-zinc-500">安装目录</span>
          <Text value={o.install_dir} onChange={(v) => set({ install_dir: v })} />
        </label>
        <label className="block">
          <span className="mb-1 block text-xs text-zinc-500">采集间隔（秒）</span>
          <Num value={o.interval_s} onChange={(v) => set({ interval_s: v ?? 3 })} min="1" max="60" />
        </label>
        <label className="block">
          <span className="mb-1 block text-xs text-zinc-500">流量月重置日</span>
          <Num
            value={o.traffic_reset_day}
            onChange={(v) => set({ traffic_reset_day: v ?? 1 })}
            min="1"
            max="31"
          />
        </label>
        <label className="block">
          <span className="mb-1 block text-xs text-zinc-500">只统计这些网卡</span>
          <Text value={o.net_include} onChange={(v) => set({ net_include: v })} placeholder="eth0, ens*" />
        </label>
        <label className="block">
          <span className="mb-1 block text-xs text-zinc-500">排除这些网卡</span>
          <Text value={o.net_exclude} onChange={(v) => set({ net_exclude: v })} placeholder="docker*, veth*" />
        </label>
      </div>
      <div className="flex flex-wrap items-center gap-4">
        <Check
          checked={o.disable_auto_update}
          onChange={(v) => set({ disable_auto_update: v })}
          label="禁用自动更新"
        />
        <Check
          checked={o.include_buffcache}
          onChange={(v) => set({ include_buffcache: v })}
          label="内存计入 buff/cache"
        />
        <Check checked={o.enable_gpu} onChange={(v) => set({ enable_gpu: v })} label="启用 GPU 监控" />
        <Btn
          kind="primary"
          onClick={() => {
            if (
              installed &&
              !confirm(
                '这台机器已经装过探针。\n\n' +
                  '生成新命令会换掉它的 token，原来那个立刻作废 —— ' +
                  '这台机器会掉线，直到你用新命令重装。\n\n继续？',
              )
            )
              return
            run(
              async () =>
                setC(
                  await post<InstallCommands>(`/api/v1/admin/servers/${id}/install`, {
                    ...o,
                    net_include: toList(o.net_include),
                    net_exclude: toList(o.net_exclude),
                  }),
                ),
              () => {},
              (m) => setErr(m || null),
            )
          }}
        >
          生成安装命令
        </Btn>
      </div>
      {c && (
        <div className="space-y-2">
          <p className="text-xs text-amber-600 dark:text-amber-500">{c.note}</p>
          {(['shell', 'docker', 'compose', 'powershell'] as const).map((k) => (
            <details key={k} open={k === 'shell'}>
              <summary className="cursor-pointer text-xs text-zinc-500">{k}</summary>
              <pre className="mt-1 overflow-x-auto rounded-lg bg-black/5 p-2 text-[11px] dark:bg-white/10">
                {c[k]}
              </pre>
            </details>
          ))}
        </div>
      )}
    </div>
  )
}
