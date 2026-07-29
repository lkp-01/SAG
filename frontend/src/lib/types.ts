export type RouteRow = {
  host: string;
  app_id: string;
  connector_endpoint: string;
  require_healthy_tunnel: boolean;
};

export type IntranetUpstreamRow = {
  app_id: string;
  upstream: string;
  scheme: "http" | "https";
};

export type PolicyRow = {
  id: string;
  effect: "ALLOW" | "DENY";
  subjects: string[];
  app_id?: string | null;
  path_prefix?: string | null;
  priority: number;
};

export type LoginResponse = {
  token: string;
  user: {
    id: string;
    username: string;
    roles: string[];
    roles_display?: string[];
    display_name?: string;
    title?: string;
  };
  expires_in_sec: number;
};

export type VerifyResponse = {
  active: boolean;
  user: {
    id: string;
    username: string;
    roles: string[];
    roles_display?: string[];
    display_name?: string;
    title?: string;
  } | null;
};

export type UserRow = {
  id: string;
  username: string;
  roles: string[];
  roles_display?: string[];
  display_name?: string;
  title?: string;
};

export type UpsertUserRequest = {
  id?: string;
  username: string;
  password?: string;
  roles: string[];
  display_name?: string;
  title?: string;
  enabled?: boolean;
};
