import { useNavigate } from '@solidjs/router';
import { to } from '../links';

/** /account/… while signed in: there's nothing to do there, so go home. */
export default function AccountRedirect() {
  const navigate = useNavigate();
  navigate(to.home(), { replace: true });
  return null;
}
