//! 通知内容的模板渲染。
//!
//! 用 `minijinja`，占位符：
//! `{{server.name}}` `{{event.kind}}` `{{event.value}}` `{{event.threshold}}` `{{time}}` …

use minijinja::Environment;
use serde::Serialize;

/// 模板可用的上下文。
#[derive(Debug, Clone, Default, Serialize)]
pub struct Context {
    pub server: ServerCtx,
    pub event: EventCtx,
    /// 面板时区下的可读时间
    pub time: String,
    /// 面板地址，方便用户点进去看
    pub panel_url: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ServerCtx {
    pub name: String,
    pub country: String,
    pub os: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct EventCtx {
    /// server_offline / cpu_high / traffic_threshold …
    pub kind: String,
    /// 人类可读的事件名
    pub label: String,
    /// 当前值（已带单位）
    pub value: String,
    /// 阈值（已带单位）
    pub threshold: String,
}

/// 默认模板。用户可以在后台覆盖。
pub const DEFAULT_TITLE: &str = "{{ server.name }} · {{ event.label }}";
pub const DEFAULT_BODY: &str = "\
机器：{{ server.name }}{% if server.country %} ({{ server.country }}){% endif %}
事件：{{ event.label }}
当前：{{ event.value }}{% if event.threshold %}（阈值 {{ event.threshold }}）{% endif %}
时间：{{ time }}{% if panel_url %}
面板：{{ panel_url }}{% endif %}";

/// 渲染。
///
/// **模板出错时回落到一个能用的默认串，而不是让通知发不出去** ——
/// 用户写错一个花括号不该导致机器挂了也没人知道。
pub fn render(template: &str, ctx: &Context) -> String {
    let env = Environment::new();
    // 通知内容是纯文本/markdown，不需要 HTML 自动转义；
    // 各渠道自己按目标格式转义（如 telegram::escape_html）
    match env.template_from_str(template).and_then(|t| t.render(ctx)) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("通知模板渲染失败，回落默认格式: {e}");
            format!(
                "{} · {}：{}",
                ctx.server.name, ctx.event.label, ctx.event.value
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Context {
        Context {
            server: ServerCtx {
                name: "RackNerd-LA".into(),
                country: "US".into(),
                os: "Debian 12".into(),
            },
            event: EventCtx {
                kind: "cpu_high".into(),
                label: "CPU 使用率过高".into(),
                value: "95.2%".into(),
                threshold: "90%".into(),
            },
            time: "2026-09-08 12:00:00".into(),
            panel_url: "https://panel.example.com".into(),
        }
    }

    #[test]
    fn default_templates_render_all_fields() {
        let title = render(DEFAULT_TITLE, &ctx());
        assert_eq!(title, "RackNerd-LA · CPU 使用率过高");

        let body = render(DEFAULT_BODY, &ctx());
        for want in [
            "RackNerd-LA",
            "(US)",
            "CPU 使用率过高",
            "95.2%",
            "阈值 90%",
            "2026-09-08",
        ] {
            assert!(body.contains(want), "正文缺少 {want}：\n{body}");
        }
    }

    #[test]
    fn optional_fields_are_omitted_not_left_blank() {
        // 没有阈值的事件（如上线/下线）不该出现「（阈值 ）」这种空壳
        let mut c = ctx();
        c.event.threshold = String::new();
        c.server.country = String::new();
        c.panel_url = String::new();
        let body = render(DEFAULT_BODY, &c);
        assert!(!body.contains("阈值"), "无阈值时不该出现阈值行：\n{body}");
        assert!(!body.contains("()"), "无国家时不该出现空括号");
        assert!(!body.contains("面板："));
    }

    #[test]
    fn broken_template_falls_back_instead_of_dropping_the_notification() {
        // 用户写错一个花括号，不该导致机器挂了也没人知道
        let out = render("{{ unclosed", &ctx());
        assert!(
            out.contains("RackNerd-LA"),
            "回落内容必须仍带关键信息：{out}"
        );
        assert!(out.contains("95.2%"));
    }

    #[test]
    fn unknown_variable_renders_empty_not_error() {
        let out = render("{{ nope }}|{{ server.name }}", &ctx());
        assert_eq!(out, "|RackNerd-LA");
    }

    #[test]
    fn custom_template_is_honoured() {
        let out = render(
            "[{{ event.kind }}] {{ server.name }} = {{ event.value }}",
            &ctx(),
        );
        assert_eq!(out, "[cpu_high] RackNerd-LA = 95.2%");
    }
}
