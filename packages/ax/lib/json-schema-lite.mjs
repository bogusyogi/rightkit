// Self-contained JSON-Schema (draft-07-ish subset) validator. No dependency on
// ajv or any external validator: this package ships zero runtime deps by
// contract, so the subset below (type/enum/const/pattern/length/range/items/
// properties/required/additionalProperties-as-bool-or-schema/allOf) is
// generalized from the pattern already proven in Cortex's evals/ax runner.
// It is deliberately not full JSON-Schema: no $ref, no anyOf/oneOf resolution
// across remote schemas. Sufficient for the vendored agent-plugins schemas,
// the .rightax.json manifest, and MCP outputSchema/structuredContent checks.

export function deepEqual(left, right) {
  if (left === right) return true;
  if (typeof left !== typeof right) return false;
  if (left === null || right === null) return false;
  if (Array.isArray(left) && Array.isArray(right)) {
    return left.length === right.length && left.every((item, index) => deepEqual(item, right[index]));
  }
  if (typeof left === "object" && typeof right === "object") {
    const leftKeys = Object.keys(left);
    const rightKeys = Object.keys(right);
    return leftKeys.length === rightKeys.length && leftKeys.every((key) => deepEqual(left[key], right[key]));
  }
  return false;
}

function matchesType(value, type) {
  switch (type) {
    case "string": return typeof value === "string";
    case "number": return typeof value === "number";
    case "integer": return typeof value === "number" && Number.isInteger(value);
    case "boolean": return typeof value === "boolean";
    case "null": return value === null;
    case "array": return Array.isArray(value);
    case "object": return value !== null && typeof value === "object" && !Array.isArray(value);
    default: return true;
  }
}

export function validateAgainstSchema(value, schema) {
  const errors = [];
  const walk = (current, currentSchema, path) => {
    if (currentSchema === true || currentSchema === undefined) return;
    if (currentSchema === false) { errors.push(`${path}: schema forbids value`); return; }
    if (currentSchema.const !== undefined && !deepEqual(current, currentSchema.const)) errors.push(`${path}: expected const ${JSON.stringify(currentSchema.const)}`);
    if (currentSchema.enum !== undefined && !currentSchema.enum.some((entry) => deepEqual(current, entry))) errors.push(`${path}: value not in enum`);
    if (currentSchema.type !== undefined) {
      const types = Array.isArray(currentSchema.type) ? currentSchema.type : [currentSchema.type];
      if (!types.some((type) => matchesType(current, type))) { errors.push(`${path}: expected type ${types.join("|")}`); return; }
    }
    if (currentSchema.allOf) currentSchema.allOf.forEach((sub) => walk(current, sub, path));
    if (currentSchema.anyOf && !currentSchema.anyOf.some((sub) => validateAgainstSchema(current, sub).valid)) errors.push(`${path}: matches no anyOf branch`);
    if (typeof current === "string") {
      if (currentSchema.minLength !== undefined && current.length < currentSchema.minLength) errors.push(`${path}: shorter than minLength ${currentSchema.minLength}`);
      if (currentSchema.maxLength !== undefined && current.length > currentSchema.maxLength) errors.push(`${path}: longer than maxLength ${currentSchema.maxLength}`);
      if (currentSchema.pattern !== undefined && !new RegExp(currentSchema.pattern).test(current)) errors.push(`${path}: pattern mismatch (${currentSchema.pattern})`);
    }
    if (typeof current === "number") {
      if (currentSchema.minimum !== undefined && current < currentSchema.minimum) errors.push(`${path}: below minimum ${currentSchema.minimum}`);
      if (currentSchema.maximum !== undefined && current > currentSchema.maximum) errors.push(`${path}: above maximum ${currentSchema.maximum}`);
    }
    if (Array.isArray(current)) {
      if (currentSchema.minItems !== undefined && current.length < currentSchema.minItems) errors.push(`${path}: fewer than minItems ${currentSchema.minItems}`);
      if (currentSchema.maxItems !== undefined && current.length > currentSchema.maxItems) errors.push(`${path}: more than maxItems ${currentSchema.maxItems}`);
      if (currentSchema.uniqueItems === true) {
        const seen = [];
        for (const item of current) if (seen.some((entry) => deepEqual(entry, item))) { errors.push(`${path}: duplicate item violates uniqueItems`); break; } else seen.push(item);
      }
      if (currentSchema.items) current.forEach((item, index) => walk(item, currentSchema.items, `${path}[${index}]`));
    }
    if (current !== null && typeof current === "object" && !Array.isArray(current)) {
      const declared = new Set(Object.keys(currentSchema.properties ?? {}));
      if (currentSchema.properties) Object.entries(currentSchema.properties).forEach(([key, sub]) => { if (current[key] !== undefined) walk(current[key], sub, `${path}.${key}`); });
      if (currentSchema.required) currentSchema.required.forEach((key) => { if (current[key] === undefined) errors.push(`${path}: missing required property ${key}`); });
      if (currentSchema.additionalProperties === false) {
        Object.keys(current).forEach((key) => { if (!declared.has(key)) errors.push(`${path}: unexpected property ${key}`); });
      } else if (currentSchema.additionalProperties && typeof currentSchema.additionalProperties === "object") {
        Object.entries(current).forEach(([key, sub]) => { if (!declared.has(key)) walk(sub, currentSchema.additionalProperties, `${path}.${key}`); });
      }
    }
  };
  walk(value, schema, "$");
  return { valid: errors.length === 0, errors };
}
