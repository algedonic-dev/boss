<script lang="ts">
  // Recursive node for the org-chart tree view. Renders one
  // employee as a card and nests their direct reports below.
  import Link from '../ui/Link.svelte';
  import { humanizeClassCode, type Employee } from './types';
  import { href } from '../router';
  import { entityHref } from '../ui/entity-href';

  type Props = {
    employee: Employee;
    childrenByManager: Map<string, Employee[]>;
    depth?: number;
  };

  let { employee, childrenByManager, depth = 0 }: Props = $props();

  let directs = $derived(childrenByManager.get(employee.id) ?? []);
</script>

<div class="org-node" style:--depth={depth}>
  <div class="org-card">
    <Link to={entityHref('employee', employee.id)}>
      {employee.name}
    </Link>
    <div class="org-role">{humanizeClassCode(employee.role)}</div>
    {#if directs.length > 0}
      <div class="org-meta">
        {directs.length} report{directs.length === 1 ? '' : 's'}
      </div>
    {/if}
  </div>

  {#if directs.length > 0}
    <ul class="org-children">
      {#each directs as child (child.id)}
        <li>
          <svelte:self
            employee={child}
            {childrenByManager}
            depth={depth + 1}
          />
        </li>
      {/each}
    </ul>
  {/if}
</div>

<style>
  .org-node {
    --indent: calc(var(--depth, 0) * 1.5rem);
  }

  .org-card {
    display: inline-flex;
    flex-direction: column;
    gap: 0.125rem;
    padding: 0.5rem 0.85rem;
    border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.08));
    border-radius: 0.5rem;
    background: var(--surface-1, #fff);
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.04);
    min-width: 14rem;
  }

  .org-role {
    font-size: 0.85rem;
    color: var(--text-muted, #555);
  }

  .org-meta {
    font-size: 0.75rem;
    color: var(--text-muted, #888);
  }

  .org-children {
    list-style: none;
    margin: 0.4rem 0 0 0;
    padding: 0 0 0 1.25rem;
    border-left: 1px dashed var(--border-soft, rgba(0, 0, 0, 0.15));
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
  }

  .org-children > li {
    margin: 0;
  }
</style>
