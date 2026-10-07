<script lang="ts">
  import { navigate } from '@boss/web-kit/nav';
  import { safeLinkHref } from '@boss/web-kit/links';
  import { renderRichMarkdown } from './richMarkdown';

  type Props = {
    body: string;
    employeeNames?: ReadonlyMap<string, string>;
    className?: string;
  };
  let { body, employeeNames, className = '' }: Props = $props();

  let rendered = $derived(renderRichMarkdown(body, employeeNames));

  function followInternalLinks(node: HTMLElement) {
    const click = (event: MouseEvent) => {
      const target = event.target instanceof Element ? event.target.closest('a') : null;
      if (!target || !node.contains(target)) return;
      event.stopPropagation();
      if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey || event.button !== 0) return;
      const path = target.getAttribute('href');
      if (!path?.startsWith('/') || safeLinkHref(path) === null) return;
      event.preventDefault();
      navigate(path);
    };
    node.addEventListener('click', click);
    return { destroy: () => node.removeEventListener('click', click) };
  }
</script>

<div class={className} use:followInternalLinks>{@html rendered}</div>
