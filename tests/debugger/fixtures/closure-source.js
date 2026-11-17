function outer(seed) {
  return () => {
    const total = seed + 1;
    return total;
  };
}
