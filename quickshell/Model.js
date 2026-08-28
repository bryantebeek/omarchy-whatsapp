.pragma library

function shallowCopy(value) {
  var output = {}
  value = value || {}
  for (var key in value) output[key] = value[key]
  return output
}

function normalizedJid(value) {
  var raw = String(value || "").trim()
  if (!raw) return ""
  if (raw.indexOf("@") >= 0) return raw
  var digits = raw.replace(/[^0-9]/g, "")
  return digits ? digits + "@s.whatsapp.net" : ""
}

function friendlyName(name, jid) {
  var value = String(name || "").trim()
  var address = String(jid || "").trim()
  if (value && value !== address && value.indexOf("@lid") < 0) return value
  var local = address.split("@")[0]
  if (address.indexOf("@s.whatsapp.net") > 0 && local) return "+" + local
  if (address.indexOf("@g.us") > 0)
    return "Group · " + local.substr(Math.max(0, local.length - 6))
  if (address.indexOf("@lid") > 0)
    return "Contact · " + local.substr(Math.max(0, local.length - 4))
  return value || address || "Conversation"
}

function contactPhoneNumber(phoneNumber, jid) {
  var value = String(phoneNumber || "").trim()
  var address = String(jid || "").trim()
  if (!value && address.indexOf("@s.whatsapp.net") > 0)
    value = address.split("@")[0]
  var digits = value.replace(/[^0-9]/g, "")
  return digits ? "+" + digits : ""
}

function initials(name, jid) {
  var text = friendlyName(name, jid).replace(/^\+/, "").trim()
  var parts = text.split(/\s+/)
  if (!parts.length || !parts[0]) return "?"
  if (parts.length === 1) return parts[0].substr(0, 2).toUpperCase()
  return (parts[0].charAt(0) + parts[parts.length - 1].charAt(0)).toUpperCase()
}

function shortTime(seconds) {
  var value = Number(seconds || 0)
  if (!value) return ""
  var date = new Date(value * 1000)
  var today = new Date()
  if (date.toDateString() === today.toDateString())
    return date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })
  var age = today.getTime() - date.getTime()
  if (age < 6 * 86400000)
    return date.toLocaleDateString([], { weekday: "short" })
  return date.toLocaleDateString([], { day: "2-digit", month: "short" })
}

function messageTime(seconds) {
  var value = Number(seconds || 0)
  if (!value) return ""
  return new Date(value * 1000).toLocaleTimeString([], {
    hour: "2-digit", minute: "2-digit"
  })
}

function mediaDuration(seconds) {
  var value = Math.max(0, Math.floor(Number(seconds || 0)))
  var hours = Math.floor(value / 3600)
  var minutes = Math.floor((value % 3600) / 60)
  var remainder = value % 60
  var paddedSeconds = remainder < 10 ? "0" + remainder : String(remainder)
  if (!hours) return minutes + ":" + paddedSeconds
  var paddedMinutes = minutes < 10 ? "0" + minutes : String(minutes)
  return hours + ":" + paddedMinutes + ":" + paddedSeconds
}

function fileSize(bytes) {
  var value = Math.max(0, Number(bytes || 0))
  if (!value) return "Unknown size"
  var units = ["B", "KB", "MB", "GB"]
  var unit = 0
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024
    unit++
  }
  var precision = unit === 0 || value >= 10 ? 0 : 1
  return value.toFixed(precision) + " " + units[unit]
}

function documentDetails(mimeType, bytes, pageCount) {
  var mime = String(mimeType || "application/octet-stream")
  var type = mime === "application/pdf" ? "PDF" : mime.split("/").pop().toUpperCase()
  var details = [type || "Document", fileSize(bytes)]
  var pages = Math.max(0, Number(pageCount || 0))
  if (pages) details.push(pages + (pages === 1 ? " page" : " pages"))
  return details.join(" · ")
}

function escapeHtml(value) {
  return String(value || "")
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;")
}

function escapedMessageText(value) {
  return escapeHtml(value).replace(/\r\n|\r|\n/g, "<br>")
}

function characterCount(value, character) {
  var count = 0
  for (var i = 0; i < value.length; i++)
    if (value.charAt(i) === character) count++
  return count
}

function trimmedLink(value) {
  var url = String(value || "")
  var suffix = ""
  var pairs = { ")": "(", "]": "[", "}": "{" }
  while (url) {
    var last = url.charAt(url.length - 1)
    var shouldTrim = /[.,!?;:]/.test(last)
    if (pairs[last])
      shouldTrim = characterCount(url, last) > characterCount(url, pairs[last])
    if (!shouldTrim) break
    suffix = last + suffix
    url = url.substr(0, url.length - 1)
  }
  return { url: url, suffix: suffix }
}

function linkifiedMessage(value, linkColor) {
  var input = String(value || "")
  var pattern = /(?:https?:\/\/|www\.)[^\s<>"']+/gi
  var output = ""
  var cursor = 0
  var match
  while ((match = pattern.exec(input)) !== null) {
    var parts = trimmedLink(match[0])
    if (!parts.url) continue
    var href = parts.url.indexOf("www.") === 0
      ? "https://" + parts.url : parts.url
    output += escapedMessageText(input.slice(cursor, match.index))
    output += "<a href=\"" + escapeHtml(href) + "\"><font color=\""
      + escapeHtml(String(linkColor || "")) + "\">"
      + escapedMessageText(parts.url) + "</font></a>"
    output += escapedMessageText(parts.suffix)
    cursor = pattern.lastIndex
  }
  output += escapedMessageText(input.slice(cursor))
  return output
}

function connectionLabel(state) {
  var value = String(state || "starting")
  if (value === "connected") return "Connected"
  if (value === "pairing") return "Link your phone"
  if (value === "logged_out") return "Unlinked"
  if (value === "disconnected") return "Reconnecting"
  if (value === "error") return "Connection error"
  return "Starting"
}
