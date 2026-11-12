/**
 * Turn a raw connection-test error (OpenSSH stderr or a russh message) into a
 * short reason, a hint about what to check, and the original detail.
 * Reasons and hints are source strings for `t()`; the detail stays verbatim.
 */
import { msg } from "../../i18n";

export interface ConnectionErrorInfo {
  reason: string;
  hint: string | null;
  detail: string;
}

const PREFIX = /^测试连接失败[：:]\s*/;
/** OpenSSH notices that are not the cause of a failure. */
const NOISE = /^\s*(Warning: Permanently added .* to the list of known hosts\.?|\*\* WARNING: connection is not using a post-quantum|\*\* This session may be vulnerable|\*\* The server may need to be upgraded)/i;

const RULES: Array<{ test: RegExp; reason: string; hint: string }> = [
  {
    test: /REMOTE HOST IDENTIFICATION HAS CHANGED|Host key verification failed|主机密钥/i,
    reason: msg("主机密钥校验失败"),
    hint: msg("服务器的主机密钥和本机记录不一致。确认服务器确实重装或更换过后，从 known_hosts 中删除旧记录再试。"),
  },
  {
    test: /Permission denied|Authentication failed|Too many authentication failures|认证被拒绝|认证失败|用户名或密码错误/i,
    reason: msg("认证失败"),
    hint: msg("检查用户名、密码或私钥是否正确，并确认服务器允许这种登录方式。"),
  },
  {
    test: /Load key|Identity file .* not accessible|invalid format|bad permissions|UNPROTECTED PRIVATE KEY|无法加载私钥/i,
    reason: msg("无法读取私钥"),
    hint: msg("检查私钥路径和格式；私钥文件权限需要只有自己可读（chmod 600）。"),
  },
  {
    test: /Could not resolve hostname|Name or service not known|nodename nor servname|No such host|failed to lookup|getaddrinfo/i,
    reason: msg("无法解析主机名"),
    hint: msg("检查主机地址是否拼写正确，或网络的 DNS 是否可用。"),
  },
  {
    test: /Connection refused/i,
    reason: msg("连接被拒绝"),
    hint: msg("该端口没有 SSH 服务在监听。确认端口号正确、sshd 已启动。"),
  },
  {
    test: /timed out|timeout|超时/i,
    reason: msg("连接超时"),
    hint: msg("确认服务器在线，主机和端口正确，防火墙或安全组已放行该端口。"),
  },
  {
    test: /No route to host|Network is unreachable|Host is down/i,
    reason: msg("网络不可达"),
    hint: msg("本机到服务器的网络不通，检查网络、VPN 或跳板机设置。"),
  },
  {
    test: /Connection reset|Connection closed|kex_exchange_identification|closed by remote host/i,
    reason: msg("连接被服务器断开"),
    hint: msg("服务器在握手阶段关闭了连接，可能是 IP 被拦截、连接数超限或 sshd 配置限制。"),
  },
];

export function describeConnectionError(raw: string): ConnectionErrorInfo {
  const detail = raw
    .replace(/^Error:\s*/, "")
    .replace(PREFIX, "")
    .split(/\r?\n/)
    .filter((line) => !NOISE.test(line))
    .join("\n")
    .trim();
  const rule = RULES.find((item) => item.test.test(detail));
  return {
    reason: rule?.reason ?? msg("测试连接失败"),
    hint: rule?.hint ?? null,
    detail,
  };
}
