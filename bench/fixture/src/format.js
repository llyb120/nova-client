function shout(text) {
  return String(text).toUpperCase();
}

function bullet(lines) {
  return lines.map((line) => `- ${line}`).join("\n");
}

function money(amount) {
  return `¥${amount.toFixed(2)}`;
}

module.exports = { shout, bullet, money };
