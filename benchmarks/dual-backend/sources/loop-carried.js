function loopCarried(start, limit, step) {
  let current = start;
  while (current !== limit) {
    current = Number(current + step);
  }
  return current;
}
