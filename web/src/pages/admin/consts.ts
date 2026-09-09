/** 后台下拉项。**值必须和后端解析器一一对应**，改任何一处都要同步另一处。 */

/** `domain/billing.rs::Cycle::parse` */
export const CYCLES: [string, string][] = [
  ['monthly', '月付'],
  ['quarterly', '季付'],
  ['semiannual', '半年付'],
  ['annual', '年付'],
  ['biennial', '两年付'],
  ['triennial', '三年付'],
  ['onetime', '一次性'],
  ['custom', '自定义天数'],
]

/** `domain/traffic.rs::CalcMode::parse` —— R5 要求的五种统计方式 */
export const CALC_MODES: [string, string][] = [
  ['sum', '总和（上行 + 下行）'],
  ['max', '取较大（上行、下行取大者）'],
  ['min', '取较小'],
  ['upload', '仅上行'],
  ['download', '仅下行'],
]

/**
 * 自动汇率覆盖的 30 种货币。不在此列的（RUB/TWD/VND/UAH/ARS/AED 等）
 * 仍可填写，但必须在「汇率」页手工设定汇率，否则该机器不计入总价值。
 *
 * 界面上**不出现具体的汇率提供方名字**：换源是实现细节，
 * 不该逼着用户去理解「Frankfurter 是什么」。
 */
export const CURRENCIES: [string, string][] = [
  'USD,EUR,CNY,GBP,HKD,JPY,SGD,AUD,CAD,KRW,INR,TRY,BRL,CHF,SEK,NOK,DKK,PLN,CZK,HUF,RON,ILS,MXN,MYR,NZD,PHP,THB,ZAR,IDR,ISK',
]
  .join('')
  .split(',')
  .map((c) => [c, c])

export const PING_KINDS: [string, string][] = [
  ['icmp', 'ICMP（需内核允许非特权 ping）'],
  ['tcp', 'TCP 连接'],
  ['http', 'HTTP(S)'],
]

export const CHANNEL_KINDS: [string, string][] = [
  ['telegram', 'Telegram'],
  ['email', '邮件 (SMTP)'],
  ['wecom', '企业微信'],
  ['lark', '飞书'],
]
