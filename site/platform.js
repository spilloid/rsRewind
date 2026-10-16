/* Suggest an OS locally. Architecture is never inferred; all choices remain visible. */
(() => {
  const platform = (navigator.userAgentData && navigator.userAgentData.platform) || navigator.platform || navigator.userAgent || '';
  if (/android|iphone|ipad/i.test(navigator.userAgent) || (/mac/i.test(platform) && navigator.maxTouchPoints > 1)) return;
  const choices = /win/i.test(platform)
    ? ['Download for Windows · x64', 'https://github.com/spilloid/rsRewind/releases/download/v0.1.2/rsrewind-v0.1.2-x64.msi', 'Windows 11 · x64 · v0.1.2 · signed installer']
    : /mac/i.test(platform)
      ? ['Download for Mac · Apple Silicon', 'https://github.com/spilloid/rsRewind/releases/download/v0.1.2/rsrewind-v0.1.2-macos-arm64.dmg', 'macOS 14+ · Apple Silicon · v0.1.2 · disk image, not notarized']
      : /linux/i.test(platform) && !/android/i.test(navigator.userAgent)
        ? ['Download for Linux · x86-64 AppImage', 'https://github.com/spilloid/rsRewind/releases/download/v0.1.2/rsrewind-v0.1.2-linux-x86_64.AppImage', 'Linux x86-64 · v0.1.2 · recording needs KDE Plasma 6 on Wayland']
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
