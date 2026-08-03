// Triggers a browser file download for a Blob or a direct URL/href string.

export function triggerDownload(source: Blob | string, filename: string): void {
  const isBlob = typeof Blob !== "undefined" && source instanceof Blob;
  const url = isBlob ? URL.createObjectURL(source as Blob) : (source as string);

  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = filename;
  anchor.style.display = "none";
  document.body.appendChild(anchor);
  anchor.click();
  document.body.removeChild(anchor);

  if (isBlob) {
    setTimeout(() => URL.revokeObjectURL(url), 0);
  }
}
