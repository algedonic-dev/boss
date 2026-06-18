// Shared policy-rule types. Extracted so EditPolicyFlyout can import
// without pulling a full component (Svelte doesn't re-export type
// declarations across `.svelte` files).

export type Scope =
  | 'none'
  | 'self'
  | 'territory'
  | 'team'
  | 'all'
  | { department: string };

export type PolicyRule = {
  id: string;
  role: string;
  resource: string;
  action: string;
  scope: Scope;
  active: boolean;
};
