function createCart() {
  const items = [];
  return {
    addItem(name, price) {
      items.push({ name, price });
    },
    removeItem(name) {
      const index = items.findIndex((item) => item.name === name);
      if (index >= 0) items.splice(index, 1);
    },
    total() {
      return items.reduce((sum, item) => sum + item.price, 0);
    },
    list() {
      return items.map((item) => `${item.name}: ${item.price}`);
    },
    size() {
      return items.length;
    },
  };
}

module.exports = { createCart };
