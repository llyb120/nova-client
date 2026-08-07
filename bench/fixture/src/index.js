const { greet } = require("./greet");
const { shout, money } = require("./format");
const { createCart } = require("./cart");

const cart = createCart();
cart.addItem("苹果", 3.5);
cart.addItem("牛奶", 12);

console.log(shout(greet("world")));
console.log(money(cart.total()));
