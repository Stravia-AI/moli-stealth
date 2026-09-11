# Moli 浏览器网络

Moli 的浏览器网络能力将传输行为与对外浏览器身份协调起来，同时区分底层传输能力和浏览器策略。

## Language

**传输指纹**：
远端能够从 TLS 握手及 HTTP 协议交互中观察到的客户端特征，不等同于 UA 字符串或完整浏览器指纹。
_Avoid_: UA 伪装、完整浏览器身份

**浏览器身份**：
Moli 在请求头和 JavaScript 可见信息中对外声明的浏览器品牌、版本、平台及相关身份信息。
_Avoid_: 传输指纹、UA 字符串

**Stealth**：
Moli 可选的联动伪装能力，为传输指纹与浏览器身份提供协调一致的基线，允许用户显式覆盖。这里不包含屏幕、GPU、Canvas 或音频伪装，也不承诺对检测不可见。
_Avoid_: 仅修改 UA、完整浏览器伪装

**Moli 嵌入式 SDK**：
供 Rust 宿主使用 Moli 浏览器、页面、JavaScript 求值、独立 HTTP 传输及 Cookie 能力的通用开发接口，以 StraviaPlatform 现有用法作为最低能力范围，不包含宿主的正文抽取、搜索就绪条件或公网地址策略。
_Avoid_: Stravia 专用抓取 SDK、完整内部 API 镜像
