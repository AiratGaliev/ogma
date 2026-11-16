const identifier = "π-value";
const match = /(?<major>\d+)\.(?<minor>\d+)/u.exec("2026.11");
const emoji = /\p{Extended_Pictographic}/u.test("🚀");

void [identifier, match?.groups?.major, emoji];
