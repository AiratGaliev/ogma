let total = 10;
const obj = {
  value: 42,
  get accessor() { throw new Error("must not be called"); },
};
const updater = () => {
  const increment = 5;
  const next = total + increment;
  total = next;
  return total;
};
const result = updater();
return result;
