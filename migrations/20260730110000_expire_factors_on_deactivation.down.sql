DROP TRIGGER IF EXISTS users_expire_factors_on_deactivation ON auth.users;
DROP FUNCTION IF EXISTS auth.expire_factors_on_deactivation();
