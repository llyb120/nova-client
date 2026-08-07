const assert = require("assert");
const { greet } = require("../src/greet");
const { shout, bullet, money } = require("../src/format");
const { createCart } = require("../src/cart");

assert.strictEqual(greet("world"), "hello, world!");
assert.strictEqual(shout("abc"), "ABC");
assert.strictEqual(bullet(["a", "b"]), "- a\n- b");
assert.strictEqual(money(3.5), "¥3.50");

const cart = createCart();
cart.addItem("苹果", 3.5);
cart.addItem("牛奶", 12);
assert.strictEqual(cart.size(), 2);
assert.strictEqual(cart.total(), 15.5);
cart.removeItem("苹果");
assert.strictEqual(cart.total(), 12);
assert.deepStrictEqual(cart.list(), ["牛奶: 12"]);

console.log("all tests passed");
