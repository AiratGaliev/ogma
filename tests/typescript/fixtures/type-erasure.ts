import type { MissingType } from "./missing.js";
type Value = number;
interface Box<T> { value: T }
function identity<T>(value: T): T { return value; }
const typed = identity(40)! satisfies Value;
export const result: Value = typed + 2;
export type { Value };
