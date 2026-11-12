import { useState } from "react";
import type { UserMessage } from "../../shared/types";

function AttachedImage({ source, index }: { source: string; index: number }) {
  const [failed, setFailed] = useState(false);
  // Render embedded raster images only; opening a trace must not contact external hosts.
  const embedded = /^data:image\/(?:png|jpeg|jpg|webp|gif|avif|bmp);base64,/i.test(source);
  if (!embedded || failed) return <div>Image {index + 1} unavailable</div>;
  return (
    <img
      className="user-message__image"
      src={source}
      alt={`User attachment ${index + 1}`}
      loading="lazy"
      onError={() => setFailed(true)}
    />
  );
}

export function UserMessageContent({ message }: { message: UserMessage }) {
  return (
    <div className="user-message__body">
      {message.text && <div className="user-message__text">{message.text}</div>}
      {message.images.map((source, index) => (
        <AttachedImage key={source} source={source} index={index} />
      ))}
    </div>
  );
}
