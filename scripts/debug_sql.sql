/* SELECT file_name, file_device, COUNT(*) */
/* FROM bep_block_info */
/* WHERE TRUE */
/* GROUP BY file_device, file_name */

/* SELECT f.*, bi.* */
/* FROM bep_file_info f */
/*     JOIN bep_block_info bi ON f.name = bi.file_name AND f.folder = bi.file_folder AND f.device = bi.file_device */
/* WHERE f.folder = 'orig_dir' AND f.device = 'RFJWU2I-J6ULCCU-36JJOGP-YWEBZAB-T5KHSUO-ROXYHCJ-CUSRTBY-WKZHNQY' */

/* SELECT f.name,v.id, v.value */
/* FROM bep_file_info f */
/*     JOIN bep_file_version v ON f.name = v.file_name AND f.folder = v.file_folder AND f.device = v.file_device */
/* WHERE f.folder = 'orig_dir' AND f.device = 'RFJWU2I-J6ULCCU-36JJOGP-YWEBZAB-T5KHSUO-ROXYHCJ-CUSRTBY-WKZHNQY' */

-- -- query to find the files that haven't been stored locally yet
-- SELECT fin.device, fin.folder, fin.name
-- FROM bep_file_location flo RIGHT JOIN bep_file_info fin ON
--  flo.loc_folder = fin.folder AND flo.loc_device = fin.device AND flo.loc_name = fin.name
-- GROUP BY fin.device, fin.folder, fin.name
-- HAVING count(flo.loc_name)
-- ;

/* SELECT * */
/* FROM bep_file_location flo RIGHT JOIN bep_file_info fin ON */
/*  flo.loc_folder = fin.folder AND flo.loc_device = fin.device AND flo.loc_name = fin.name */
/* WHERE fin.device = 'DC3KS6Y-XZWHD4F-T2L5QY6-LJ2GR7X-THN23CX-33LLN46-WZ7FAQO-AVE3BQV' */
/* ; */

 
-- select fin.folder, fin.device, fin.name, fin.sequence, fin.rowid
-- from bep_file_info fin
-- WHERE fin.device = 'DC3KS6Y-XZWHD4F-T2L5QY6-LJ2GR7X-THN23CX-33LLN46-WZ7FAQO-AVE3BQV'
-- order by fin.name
-- ;

-- SELECT lc.cache_folder, lc.cache_file_name, lc.cache_device, lc.timestamp_added, fi.size
-- FROM bep_local_cache lc JOIN bep_file_info fi ON TRUE
--   AND lc.cache_folder = fi.folder
--   AND lc.cache_file_name = fi.name
--   AND lc.cache_device = fi.device
-- WHERE fi.type = 0
-- ;

-- SELECT DISTINCT(f.rowid) AS f_id, f.*
-- FROM bep_folders f JOIN bep_devices d ON f.id = d.folder
-- WHERE d.id = 'QS2CM5G-P6P6WZC-2SOA2PW-NTDTTAY-PM4GUXW-KA3BW6B-6WXPNLA-LDDEYAJ'

SELECT DISTINCT(f.rowid) AS f_id, f.*
FROM bep_folders f JOIN bep_index i ON f.id = i.folder
WHERE i.device = 'QS2CM5G-P6P6WZC-2SOA2PW-NTDTTAY-PM4GUXW-KA3BW6B-6WXPNLA-LDDEYAJ'
