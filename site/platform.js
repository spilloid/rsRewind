/* Suggest an OS locally. Architecture is never inferred; all choices remain visible. */
(() => {
  const platform = (navigator.userAgentData && navigator.userAgentData.platform) || navigator.platform || navigator.userAgent || '';
  if (/android|iphone|ipad/i.test(navigator.userAgent) || (/mac/i.test(platform) && navigator.maxTouchPoints > 1)) return;
  const choices = /win/i.test(platform)
    ? ['Download for Windows · x64', 'https://github.com/spilloid/rsRewind/releases/download/v0.0.6/rsrewind-v0.0.6-x64.msi', 'Windows 11 · v0.0.6 · signed installer']
    : /mac/i.test(platform)
      ? ['Get the Mac preview · Apple Silicon', 'install.html#macos', 'macOS 14+ · Apple Silicon only · development build, not notarized']
      : /linux/i.test(platform) && !/android/i.test(navigator.userAgent)
        ? ['Download for Linux · x86-64', 'https://github.com/spilloid/rsRewind/releases/download/v0.0.6/rsrewind-v0.0.6-linux-x86_64.tar.gz', 'Recording requires KDE Plasma 6 on Wayland · v0.0.6']
        : null;
  if (!choices) return;
  document.querySelectorAll('[data-platform-download]').forEach(link => {
    link.textContent = choices[0];
    link.href = choices[1];
  });
  document.querySelectorAll('[data-platform-note]').forEach(note => {
    note.textContent = choices[2] + ' · ';
    const all = document.createElement('a');
    all.href = 'install.html#platforms';
    all.textContent = 'All platforms';
    note.appendChild(all);
  });
})();
