// Dedicated HUMAN surface, source-only until the private broker is installed.
// No provider material enters the ordinary app DOM, request or step metadata.
// The future authenticated private transport is a separate adapter obligation;
// a generic jobs PATCH, popup URL supplied as metadata or a pasted token is not it.
(function () {
  function mount(container, props) {
    const panel = document.createElement('section');
    const title = document.createElement('h3');
    title.textContent = 'Set up dedicated agent access';
    const state = document.createElement('p');
    state.textContent = 'Private setup is waiting for the broker to be installed. This step cannot enroll or install a login yet.';
    const button = document.createElement('button');
    button.type = 'button';
    button.disabled = true;
    button.textContent = 'Private setup unavailable';
    panel.append(title, state, button);
    const receipt = props.step?.metadata?.broker_receipt_id;
    if (typeof receipt === 'string' && /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(receipt)) {
      const reference = document.createElement('p');
      reference.textContent = `Broker reference: ${receipt}. Installation and runtime verification remain pending.`;
      panel.append(reference);
    }
    container.replaceChildren(panel);
    return () => container.replaceChildren();
  }
  window.__boss_register_step_plugin('private-enrollment', mount);
})();
