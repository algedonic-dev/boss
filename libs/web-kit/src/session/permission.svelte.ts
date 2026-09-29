// The reactive face of ./can for a component: `permission('create',
// 'ledger').value` is false until policy answers Allow for the session
// user, and false again the moment the session user changes, until the
// new user's answer arrives (backlog 9dad102c). Call it during component
// initialisation — it owns an $effect.

import { session } from './session.svelte';
import { can } from './can';

export function permission(action: string, resource: string): { readonly value: boolean } {
  let answer = $state(false);
  $effect(() => {
    const user = session.policyUser;
    let live = true;
    answer = false;
    void can(user, action, resource).then((allowed) => {
      if (live) answer = allowed;
    });
    return () => {
      live = false;
    };
  });
  return {
    get value() {
      return answer;
    },
  };
}
