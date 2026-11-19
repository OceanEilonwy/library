import { to } from '../links';
import { EmptyNote } from '../components/Common';

export default function NotFound() {
  return (
    <div class="page">
      <EmptyNote title="There's nothing on this shelf">
        <p>
          The page you followed doesn't exist. <a href={to.home()}>Go to Home</a>.
        </p>
      </EmptyNote>
    </div>
  );
}
