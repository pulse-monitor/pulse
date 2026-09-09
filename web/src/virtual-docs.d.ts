declare module 'virtual:docs' {
  export interface DocPage {
    slug: string
    title: string
    html: string
    headings: { id: string; text: string }[]
  }
  export const docs: DocPage[]
}
